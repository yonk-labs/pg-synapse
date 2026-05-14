//! Proc-macro support for `pg-synapse-core`.
//!
//! Provides a single derive macro, [`Tool`], that turns a plain struct (with
//! `serde::Deserialize` and `schemars::JsonSchema` derives) into a full
//! implementation of the `pg_synapse_core::Tool` trait. Tool authors then
//! only write the actual handler:
//!
//! ```ignore
//! use pg_synapse_macros::Tool;
//! use pg_synapse_core::types::{ToolCtx, ToolOutput};
//! use pg_synapse_core::error::ToolError;
//! use schemars::JsonSchema;
//! use serde::Deserialize;
//!
//! #[derive(Tool, JsonSchema, Deserialize)]
//! #[tool(name = "echo", description = "Echo the input back")]
//! struct Echo {
//!     /// The text to echo.
//!     text: String,
//! }
//!
//! impl Echo {
//!     async fn run(self, _ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
//!         Ok(ToolOutput::text(self.text))
//!     }
//! }
//! ```
//!
//! ## What the macro generates
//!
//! - Two associated consts on the struct, `TOOL_NAME` and `TOOL_DESCRIPTION`.
//! - An `impl pg_synapse_core::Tool for Self`:
//!   - `name()` returns `Self::TOOL_NAME`.
//!   - `schema()` lazily builds the schema with `schemars::schema_for!` and
//!     caches it in a `OnceLock`.
//!   - `run(input, ctx)` deserializes `input` into `Self`, returns
//!     `ToolError::InvalidInput` on failure, then awaits the user's
//!     inherent `run(self, ctx)` method.
//!
//! ## Attribute parsing
//!
//! `#[tool(name = "...", description = "...")]` is the only supported
//! attribute. Both fields are optional:
//!
//! - `name` defaults to the struct's identifier lowercased.
//! - `description` defaults to the empty string.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use darling::FromDeriveInput;
use proc_macro::TokenStream;
use quote::quote;
use syn::{DeriveInput, parse_macro_input};

/// `#[tool(...)]` attribute on the struct.
#[derive(Debug, FromDeriveInput)]
#[darling(attributes(tool), supports(struct_any))]
struct ToolAttrs {
    ident: syn::Ident,
    /// Tool name exposed to the agent. Defaults to the struct name lowercased.
    #[darling(default)]
    name: Option<String>,
    /// Human-readable description used in tool listings. Defaults to empty.
    #[darling(default)]
    description: Option<String>,
}

/// Derive `pg_synapse_core::Tool` for a struct that already implements
/// `serde::Deserialize` and `schemars::JsonSchema`.
///
/// See the crate-level docs for the full shape.
#[proc_macro_derive(Tool, attributes(tool))]
pub fn derive_tool(input: TokenStream) -> TokenStream {
    let ast = parse_macro_input!(input as DeriveInput);
    let attrs = match ToolAttrs::from_derive_input(&ast) {
        Ok(a) => a,
        Err(e) => return e.write_errors().into(),
    };

    let ident = &attrs.ident;
    let tool_name = attrs
        .name
        .unwrap_or_else(|| attrs.ident.to_string().to_lowercase());
    let description = attrs.description.unwrap_or_default();

    let expanded = quote! {
        impl #ident {
            /// Registered tool name, as advertised to the agent.
            pub const TOOL_NAME: &'static str = #tool_name;
            /// Tool description, used in documentation and agent prompts.
            pub const TOOL_DESCRIPTION: &'static str = #description;
        }

        #[::pg_synapse_core::async_trait::async_trait]
        impl ::pg_synapse_core::Tool for #ident {
            fn name(&self) -> &str {
                <#ident>::TOOL_NAME
            }

            fn schema(&self) -> &::pg_synapse_core::types::ToolSchema {
                static SCHEMA: ::std::sync::OnceLock<::pg_synapse_core::types::ToolSchema> =
                    ::std::sync::OnceLock::new();
                SCHEMA.get_or_init(|| {
                    let root = ::schemars::schema_for!(#ident);
                    ::pg_synapse_core::types::ToolSchema::from_root(root)
                })
            }

            async fn run(
                &self,
                input: ::serde_json::Value,
                ctx: &::pg_synapse_core::types::ToolCtx,
            ) -> ::std::result::Result<
                ::pg_synapse_core::types::ToolOutput,
                ::pg_synapse_core::error::ToolError,
            > {
                let parsed: Self = ::serde_json::from_value(input).map_err(|e| {
                    ::pg_synapse_core::error::ToolError::InvalidInput {
                        name: <#ident>::TOOL_NAME.to_string(),
                        reason: e.to_string(),
                    }
                })?;
                parsed.run(ctx).await
            }
        }
    };

    expanded.into()
}

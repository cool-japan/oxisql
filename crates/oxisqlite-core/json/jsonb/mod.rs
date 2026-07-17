//! Auto-generated module structure

pub mod constants;
pub mod deleteoperation_traits;
pub mod elementtype_traits;
pub mod functions;
pub mod insertoperation_traits;
pub mod jsonb_impl;
pub mod jsonb_serialization;
pub mod jsonb_traits;
pub mod jsonb_type;
pub mod replaceoperation_traits;
pub mod searchoperation_traits;
pub mod setoperation_traits;
pub mod type_aliases;
pub mod types;

// Re-export all types
pub use jsonb_type::*;
pub use types::*;

#[cfg(test)]
mod tests;

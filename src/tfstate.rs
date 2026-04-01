use std::collections::HashMap;

/// Strongly typed representation of a Terraform state file (format version 4).
///
/// The `resources` field is left as a dynamic `facet_value::Value` because
/// resource schemas are provider-defined and unknowable at compile time.
/// All other idiomatic top-level fields are strongly typed.
#[derive(facet::Facet)]
pub struct TfState {
    /// State format version (always 4 for Terraform >= 0.12)
    pub version: u32,
    /// Version of Terraform that wrote this state
    pub terraform_version: String,
    /// Monotonically increasing serial number, incremented on every write
    pub serial: u64,
    /// Unique identifier that stays constant across the lifetime of a state file
    pub lineage: String,
    /// Named output values declared in the root module
    pub outputs: HashMap<String, TfOutput>,
    /// Resource instances (provider-defined schema, kept dynamic)
    pub resources: Vec<facet_value::Value>,
    /// Health check results (Terraform 1.5+, null when unused)
    pub check_results: Option<facet_value::Value>,
}

/// A single output value from a Terraform state.
#[derive(facet::Facet)]
pub struct TfOutput {
    /// The output value (can be any JSON type)
    pub value: facet_value::Value,
    /// Terraform type expression for the value
    #[facet(rename = "type")]
    pub type_: facet_value::Value,
    /// Whether the output was declared sensitive
    pub sensitive: bool,
}

use std::collections::HashMap;

/// Strongly typed Terraform state file (format version 4).
///
/// `resources` is a typed Vec — only the instances' `attributes` remain dynamic
/// since those are provider-defined. Everything else is strongly known.
#[derive(facet::Facet)]
pub struct TfState {
    pub version: u32,
    pub terraform_version: String,
    pub serial: u64,
    pub lineage: String,
    pub outputs: HashMap<String, TfOutput>,
    pub resources: Vec<TfResource>,
    pub check_results: Option<facet_value::Value>,
}

/// A named output value declared in the root module.
#[derive(facet::Facet)]
pub struct TfOutput {
    pub value: facet_value::Value,
    #[facet(rename = "type")]
    pub type_: facet_value::Value,
    pub sensitive: bool,
}

/// A resource block — one entry per `resource "type" "name"` declaration.
#[derive(facet::Facet)]
pub struct TfResource {
    /// "managed" for regular resources, "data" for data sources
    pub mode: String,
    #[facet(rename = "type")]
    pub type_: String,
    pub name: String,
    /// Present for resources inside a module (e.g. "module.vpc")
    pub module: Option<String>,
    pub provider: String,
    pub instances: Vec<TfInstance>,
}

impl TfResource {
    /// Full Terraform address, e.g. `module.vpc.aws_vpc.main` or `data.aws_ami.ubuntu`
    pub fn address(&self) -> String {
        let base = if self.mode == "data" {
            format!("data.{}.{}", self.type_, self.name)
        } else {
            format!("{}.{}", self.type_, self.name)
        };
        match &self.module {
            Some(m) => format!("{m}.{base}"),
            None => base,
        }
    }
}

/// One instance of a resource (multiple exist when `count` or `for_each` is used).
#[derive(facet::Facet, PartialEq)]
pub struct TfInstance {
    pub schema_version: u32,
    /// All provider-specific attributes — dynamic by design
    pub attributes: facet_value::Value,
    pub sensitive_attributes: Vec<facet_value::Value>,
    pub private: Option<String>,
    pub dependencies: Option<Vec<String>>,
}

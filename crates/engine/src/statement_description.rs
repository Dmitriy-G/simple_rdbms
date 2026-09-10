use types::DataType;

#[derive(Debug, Clone, PartialEq)]
pub struct StatementDescription {
    pub param_types: Vec<Option<DataType>>,
    pub columns: Vec<(String, Option<DataType>)>,
}

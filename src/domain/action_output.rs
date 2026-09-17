//! Action output shared by host adapters and transport implementations.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputLine {
    pub action_id: String,
    pub text: String,
}

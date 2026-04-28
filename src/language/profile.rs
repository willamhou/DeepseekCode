#[derive(Debug, Clone)]
pub struct LanguageProfile {
    pub name: String,
    pub file_priority: Vec<String>,
    pub test_commands: Vec<String>,
    pub hints: Vec<String>,
}

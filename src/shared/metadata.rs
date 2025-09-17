use once_cell::sync::Lazy;
use regex::Regex;
use std::collections::{HashMap, HashSet};

static TECHNOLOGY_PATTERNS: Lazy<HashMap<&'static str, Regex>> = Lazy::new(|| {
    let mut map = HashMap::new();

    // Programming languages
    map.insert(
        "rust",
        Regex::new(r"(?i)\b(rust|cargo|rustc|rustup)\b").unwrap(),
    );
    map.insert(
        "python",
        Regex::new(r"(?i)\b(python|pip|conda|virtualenv|pytest)\b").unwrap(),
    );
    map.insert(
        "javascript",
        Regex::new(r"(?i)\b(javascript|js|node\.js|npm|yarn|react|vue)\b").unwrap(),
    );
    map.insert(
        "typescript",
        Regex::new(r"(?i)\b(typescript|ts|tsc)\b").unwrap(),
    );
    map.insert(
        "go",
        Regex::new(r"(?i)\b(golang|go\s+run|go\s+build|go\s+mod)\b").unwrap(),
    );
    map.insert(
        "java",
        Regex::new(r"(?i)\b(java|gradle|maven|spring)\b").unwrap(),
    );
    map.insert(
        "csharp",
        Regex::new(r"(?i)\b(c#|csharp|dotnet|nuget)\b").unwrap(),
    );
    map.insert(
        "cpp",
        Regex::new(r"(?i)\b(c\+\+|cpp|cmake|clang\+\+|g\+\+)\b").unwrap(),
    );
    map.insert("c", Regex::new(r"(?i)\b(gcc|clang|make)\b").unwrap());

    // Frameworks and libraries
    map.insert(
        "react",
        Regex::new(r"(?i)\b(react|jsx|useState|useEffect)\b").unwrap(),
    );
    map.insert("vue", Regex::new(r"(?i)\b(vue\.js|vuex|nuxt)\b").unwrap());
    map.insert(
        "angular",
        Regex::new(r"(?i)\b(angular|ng\s+|\@angular)\b").unwrap(),
    );
    map.insert(
        "django",
        Regex::new(r"(?i)\b(django|python.*web)\b").unwrap(),
    );
    map.insert("flask", Regex::new(r"(?i)\bflask\b").unwrap());
    map.insert(
        "express",
        Regex::new(r"(?i)\b(express\.js|express)\b").unwrap(),
    );

    // Databases
    map.insert(
        "postgresql",
        Regex::new(r"(?i)\b(postgres|postgresql|psql)\b").unwrap(),
    );
    map.insert("mysql", Regex::new(r"(?i)\b(mysql|mariadb)\b").unwrap());
    map.insert("sqlite", Regex::new(r"(?i)\bsqlite\b").unwrap());
    map.insert(
        "mongodb",
        Regex::new(r"(?i)\b(mongodb|mongo|mongoose)\b").unwrap(),
    );
    map.insert("redis", Regex::new(r"(?i)\bredis\b").unwrap());

    // Infrastructure and DevOps
    map.insert(
        "docker",
        Regex::new(r"(?i)\b(docker|dockerfile|container)\b").unwrap(),
    );
    map.insert(
        "kubernetes",
        Regex::new(r"(?i)\b(kubernetes|k8s|kubectl|helm)\b").unwrap(),
    );
    map.insert(
        "aws",
        Regex::new(r"(?i)\b(aws|amazon.*web|ec2|s3|lambda)\b").unwrap(),
    );
    map.insert(
        "gcp",
        Regex::new(r"(?i)\b(gcp|google.*cloud|gke)\b").unwrap(),
    );
    map.insert(
        "azure",
        Regex::new(r"(?i)\b(azure|microsoft.*cloud)\b").unwrap(),
    );
    map.insert("terraform", Regex::new(r"(?i)\bterraform\b").unwrap());
    map.insert("ansible", Regex::new(r"(?i)\bansible\b").unwrap());

    // Version control and CI/CD
    map.insert(
        "git",
        Regex::new(r"(?i)\b(git|github|gitlab|bitbucket)\b").unwrap(),
    );
    map.insert(
        "cicd",
        Regex::new(r"(?i)\b(jenkins|github.*actions|gitlab.*ci|circleci|travis)\b").unwrap(),
    );

    // Web technologies
    map.insert("html", Regex::new(r"(?i)\b(html|html5)\b").unwrap());
    map.insert(
        "css",
        Regex::new(r"(?i)\b(css|css3|sass|scss|less)\b").unwrap(),
    );
    map.insert(
        "api",
        Regex::new(r"(?i)\b(api|rest|graphql|endpoint)\b").unwrap(),
    );

    // Search and data processing
    map.insert(
        "elasticsearch",
        Regex::new(r"(?i)\b(elasticsearch|elastic|kibana)\b").unwrap(),
    );
    map.insert("tantivy", Regex::new(r"(?i)\btantivy\b").unwrap());
    map.insert("lucene", Regex::new(r"(?i)\blucene\b").unwrap());

    map
});

static TOOL_PATTERNS: Lazy<HashMap<&'static str, Regex>> = Lazy::new(|| {
    let mut map = HashMap::new();

    // CLI tools and commands commonly used in Claude Code
    map.insert(
        "bash",
        Regex::new(r"(?i)\b(bash|shell|terminal|command.*line)\b").unwrap(),
    );
    map.insert(
        "grep",
        Regex::new(r"(?i)\b(grep|rg|ripgrep|search)\b").unwrap(),
    );
    map.insert("find", Regex::new(r"(?i)\b(find|locate|which)\b").unwrap());
    map.insert(
        "curl",
        Regex::new(r"(?i)\b(curl|wget|http.*request)\b").unwrap(),
    );
    map.insert("ssh", Regex::new(r"(?i)\b(ssh|scp|rsync)\b").unwrap());
    map.insert(
        "systemctl",
        Regex::new(r"(?i)\b(systemctl|systemd|service)\b").unwrap(),
    );
    map.insert(
        "vim",
        Regex::new(r"(?i)\b(vim|neovim|nvim|editor)\b").unwrap(),
    );
    map.insert(
        "tmux",
        Regex::new(r"(?i)\b(tmux|screen|session)\b").unwrap(),
    );

    map
});

static CODE_BLOCK_PATTERN: Lazy<Regex> = Lazy::new(|| Regex::new(r"```(\w+)?\n").unwrap());

static LANGUAGE_PATTERNS: Lazy<HashMap<&'static str, Regex>> = Lazy::new(|| {
    let mut map = HashMap::new();

    map.insert("rust", Regex::new(r"```rust\n").unwrap());
    map.insert("python", Regex::new(r"```python\n").unwrap());
    map.insert("javascript", Regex::new(r"```(javascript|js)\n").unwrap());
    map.insert("typescript", Regex::new(r"```(typescript|ts)\n").unwrap());
    map.insert("bash", Regex::new(r"```(bash|sh|shell)\n").unwrap());
    map.insert("json", Regex::new(r"```json\n").unwrap());
    map.insert("yaml", Regex::new(r"```(yaml|yml)\n").unwrap());
    map.insert("toml", Regex::new(r"```toml\n").unwrap());
    map.insert("sql", Regex::new(r"```sql\n").unwrap());
    map.insert("dockerfile", Regex::new(r"```dockerfile\n").unwrap());
    map.insert("html", Regex::new(r"```html\n").unwrap());
    map.insert("css", Regex::new(r"```css\n").unwrap());
    map.insert("xml", Regex::new(r"```xml\n").unwrap());

    map
});

static ERROR_PATTERNS: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\b(error|exception|failed|failure|panic|crash|bug|issue|problem|broken)\b")
        .unwrap()
});

pub struct MetadataExtractor;

impl MetadataExtractor {
    pub fn extract_technologies(content: &str) -> Vec<String> {
        let mut technologies = HashSet::new();

        for (tech, pattern) in TECHNOLOGY_PATTERNS.iter() {
            if pattern.is_match(content) {
                technologies.insert(tech.to_string());
            }
        }

        technologies.into_iter().collect()
    }

    pub fn extract_tools_mentioned(content: &str) -> Vec<String> {
        let mut tools = HashSet::new();

        for (tool, pattern) in TOOL_PATTERNS.iter() {
            if pattern.is_match(content) {
                tools.insert(tool.to_string());
            }
        }

        tools.into_iter().collect()
    }

    pub fn extract_code_languages(content: &str) -> Vec<String> {
        let mut languages = HashSet::new();

        for (lang, pattern) in LANGUAGE_PATTERNS.iter() {
            if pattern.is_match(content) {
                languages.insert(lang.to_string());
            }
        }

        languages.into_iter().collect()
    }

    pub fn has_code_blocks(content: &str) -> bool {
        CODE_BLOCK_PATTERN.is_match(content)
    }

    pub fn has_error_mentions(content: &str) -> bool {
        ERROR_PATTERNS.is_match(content)
    }

    pub fn count_words(content: &str) -> usize {
        content.split_whitespace().count()
    }

    pub fn extract_all_metadata(
        content: &str,
    ) -> (Vec<String>, Vec<String>, Vec<String>, bool, bool) {
        let technologies = Self::extract_technologies(content);
        let tools_mentioned = Self::extract_tools_mentioned(content);
        let code_languages = Self::extract_code_languages(content);
        let has_code = Self::has_code_blocks(content);
        let has_error = Self::has_error_mentions(content);
        (
            technologies,
            tools_mentioned,
            code_languages,
            has_code,
            has_error,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_technology_extraction() {
        let content = "I'm working on a Rust project with Cargo and need to use Docker containers";
        let techs = MetadataExtractor::extract_technologies(content);
        assert!(techs.contains(&"rust".to_string()));
        assert!(techs.contains(&"docker".to_string()));
    }

    #[test]
    fn test_code_detection() {
        let content_with_code = "Here's some code:\n```rust\nfn main() {}\n```";
        let content_without_code = "This is just plain text";

        assert!(MetadataExtractor::has_code_blocks(content_with_code));
        assert!(!MetadataExtractor::has_code_blocks(content_without_code));
    }

    #[test]
    fn test_error_detection() {
        let content_with_error = "I'm getting an error when running this";
        let content_normal = "Everything is working fine";

        assert!(MetadataExtractor::has_error_mentions(content_with_error));
        assert!(!MetadataExtractor::has_error_mentions(content_normal));
    }
}

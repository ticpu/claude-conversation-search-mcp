use serde_json::Value;

pub fn extract_content_from_json(json: &Value) -> String {
    println!("DEBUG: Starting content extraction");
    
    // Try message.content first (standard Claude format)
    if let Some(message) = json.get("message") {
        println!("DEBUG: Found message field");
        if let Some(content) = message.get("content") {
            println!("DEBUG: Found content field, type: {:?}", content);
            
            if let Some(text) = content.as_str() {
                println!("DEBUG: Content is string, length: {}", text.len());
                return text.to_string();
            }
            if content.is_array() {
                println!("DEBUG: Content is array with {} items", content.as_array().unwrap().len());
                let mut text_parts = Vec::new();
                for (i, part) in content.as_array().unwrap().iter().enumerate() {
                    println!("DEBUG: Part {}: {:?}", i, part);
                    if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                        println!("DEBUG: Found text in part {}, length: {}", i, text.len());
                        text_parts.push(text);
                    }
                }
                let result = text_parts.join(" ");
                println!("DEBUG: Joined result length: {}", result.len());
                return result;
            }
            println!("DEBUG: Content is neither string nor array");
        } else {
            println!("DEBUG: No content field in message");
        }
    } else {
        println!("DEBUG: No message field");
    }

    // Fallback to direct content field
    if let Some(content) = json.get("content").and_then(|v| v.as_str()) {
        println!("DEBUG: Found direct content, length: {}", content.len());
        return content.to_string();
    }

    println!("DEBUG: No content found anywhere");
    String::new()
}

fn main() {
    let json_str = std::io::read_to_string(std::io::stdin()).unwrap();
    let json: Value = serde_json::from_str(&json_str).unwrap();
    let content = extract_content_from_json(&json);
    println!("Final content length: {}", content.len());
}

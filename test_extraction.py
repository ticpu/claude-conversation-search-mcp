import json
import sys

# Read the JSONL line
line = sys.stdin.read().strip()
data = json.loads(line)

# Manual extraction following our Rust logic
if 'message' in data and 'content' in data['message']:
    content = data['message']['content']
    if isinstance(content, str):
        print(f"String content: {len(content)} chars")
        print(content[:100] + "...")
    elif isinstance(content, list):
        text_parts = []
        for part in content:
            if isinstance(part, dict) and 'text' in part:
                text_parts.append(part['text'])
        result = " ".join(text_parts)
        print(f"Array content: {len(result)} chars")
        print(result[:100] + "...")
    else:
        print(f"Unknown content type: {type(content)}")
elif 'content' in data:
    print(f"Direct content: {len(data['content'])} chars")
else:
    print("No content found")

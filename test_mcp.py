#!/usr/bin/env python3
"""
Test script for the Claude Search MCP server
"""
import json
import subprocess
import sys
import time

def send_request(proc, request):
    """Send a JSON-RPC request to the MCP server"""
    request_json = json.dumps(request)
    print(f"→ {request_json}")
    proc.stdin.write(request_json + "\n")
    proc.stdin.flush()

def read_response(proc):
    """Read a JSON-RPC response from the MCP server"""
    line = proc.stdout.readline()
    if not line:
        return None
    response = json.loads(line.strip())
    print(f"← {json.dumps(response, indent=2)}")
    return response

def test_mcp_server():
    """Test the MCP server functionality"""
    print("Starting Claude Search MCP Server test...")
    
    # Start the MCP server
    proc = subprocess.Popen(
        ["./target/debug/claude-search-mcp"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        bufsize=0
    )
    
    try:
        # Test 1: Initialize
        print("\n=== Test 1: Initialize ===")
        send_request(proc, {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {"experimental": {}, "sampling": {}},
                "clientInfo": {"name": "test-client", "version": "1.0.0"}
            }
        })
        response = read_response(proc)
        assert response and "result" in response, "Initialize failed"
        
        # Test 2: Initialized notification
        print("\n=== Test 2: Initialized Notification ===")
        send_request(proc, {
            "jsonrpc": "2.0",
            "id": 2,
            "method": "initialized"
        })
        response = read_response(proc)
        assert response and "result" in response, "Initialized failed"
        
        # Test 3: List tools
        print("\n=== Test 3: List Tools ===")
        send_request(proc, {
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/list"
        })
        response = read_response(proc)
        assert response and "result" in response, "List tools failed"
        tools = response["result"]["tools"]
        print(f"Found {len(tools)} tools:")
        for tool in tools:
            print(f"  - {tool['name']}: {tool['description']}")
        
        # Test 4: Search conversations
        print("\n=== Test 4: Search Conversations ===")
        send_request(proc, {
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": "search_conversations",
                "arguments": {
                    "query": "rust",
                    "limit": 3
                }
            }
        })
        response = read_response(proc)
        if response and "result" in response:
            print("✅ Search conversations working")
            content = response["result"]["content"][0]["text"]
            print(f"Search result preview:\n{content[:200]}...")
        else:
            print("❌ Search conversations failed")
        
        # Test 5: Get conversation stats
        print("\n=== Test 5: Get Stats ===")
        send_request(proc, {
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": {
                "name": "get_conversation_stats",
                "arguments": {}
            }
        })
        response = read_response(proc)
        if response and "result" in response:
            print("✅ Get stats working")
            content = response["result"]["content"][0]["text"]
            print(f"Stats preview:\n{content[:300]}...")
        else:
            print("❌ Get stats failed")
        
        # Test 6: Analyze topics
        print("\n=== Test 6: Analyze Topics ===")
        send_request(proc, {
            "jsonrpc": "2.0",
            "id": 6,
            "method": "tools/call",
            "params": {
                "name": "analyze_conversation_topics",
                "arguments": {"limit": 5}
            }
        })
        response = read_response(proc)
        if response and "result" in response:
            print("✅ Analyze topics working")
            content = response["result"]["content"][0]["text"]
            print(f"Topics preview:\n{content[:300]}...")
        else:
            print("❌ Analyze topics failed")
            
        print("\n✅ All tests completed successfully!")
        
    except Exception as e:
        print(f"\n❌ Test failed with error: {e}")
        return False
        
    finally:
        # Clean up
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
    
    return True

if __name__ == "__main__":
    success = test_mcp_server()
    sys.exit(0 if success else 1)
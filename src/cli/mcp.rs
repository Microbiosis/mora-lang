//! v0.75.53: mcp CLI 命令（从 main.rs 拆出，P9）。
//! 共享编译/路径辅助在 super::（cli/mod.rs）。

pub fn run_mcp_tool_list() {
    use crate::mcp_server::builtin_toolsets;

    let toolsets = builtin_toolsets();
    let mut all_tools: Vec<(&str, &str)> = Vec::new();

    for (toolset, tools) in &toolsets {
        for tool in tools {
            all_tools.push((tool, toolset));
        }
    }

    // 去重
    all_tools.sort();
    all_tools.dedup_by(|a, b| a.0 == b.0);

    println!("MCP Tools ({}):\n", all_tools.len());
    println!("{:<30} {:<15}", "TOOL", "TOOLSET");
    println!("{}", "-".repeat(45));
    for (tool, toolset) in &all_tools {
        println!("{:<30} {:<15}", tool, toolset);
    }
}

pub fn run_mcp_tool_search(query: &str) {
    use crate::mcp_server::builtin_toolsets;

    let toolsets = builtin_toolsets();
    let query_lower = query.to_lowercase();
    let mut results: Vec<(&str, &str)> = Vec::new();

    for (toolset, tools) in &toolsets {
        for tool in tools {
            if tool.to_lowercase().contains(&query_lower)
                || toolset.to_lowercase().contains(&query_lower)
            {
                results.push((tool, toolset));
            }
        }
    }

    results.sort();
    results.dedup_by(|a, b| a.0 == b.0);

    if results.is_empty() {
        println!("No tools found matching '{}'", query);
    } else {
        println!("Search results for '{}' ({}):\n", query, results.len());
        println!("{:<30} {:<15}", "TOOL", "TOOLSET");
        println!("{}", "-".repeat(45));
        for (tool, toolset) in &results {
            println!("{:<30} {:<15}", tool, toolset);
        }
    }
}

pub fn run_mcp_toolsets() {
    use crate::mcp_server::builtin_toolsets;

    let toolsets = builtin_toolsets();

    println!("MCP Toolsets ({}):\n", toolsets.len());
    println!("{:<15} {:>6} DESCRIPTION", "TOOLSET", "TOOLS");
    println!("{}", "-".repeat(60));
    for (toolset, tools) in &toolsets {
        let desc = match toolset.as_str() {
            "ai" => "AI 调用相关工具",
            "json" => "JSON 处理工具",
            "file" => "文件系统操作",
            "web" => "HTTP 请求工具",
            "default" => "默认启用的工具集",
            _ => "",
        };
        println!("{:<15} {:>6} {}", toolset, tools.len(), desc);
    }

    println!("\nUsage:");
    println!("  mora mcp --toolsets ai,json,file    # 启用指定 toolset");
    println!("  mora mcp --tools ai.chat,json.parse  # 启用指定工具");
    println!("  mora mcp --toolsets all              # 启用所有工具");
}

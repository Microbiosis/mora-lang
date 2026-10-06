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

    // v0.104.6 D186：**如实标注这是什么**。
    //
    // 修前标题写「MCP Tools (13):」—— 一个**计数**，读起来像是在报告某个
    // 系统的状态。但它读的是 `builtin_toolsets()` 这张**静态名字目录**，
    // 与真正对外暴露的工具**毫无关系**：后者由每个脚本自己用
    // `server.tool(name, schema, handler)` 注册。
    //
    // 判别性实测（同一台机器、同一个二进制）：
    //
    // ```text
    // $ mora mcp tool-list            →  MCP Tools (13)   ← 静态目录
    // $ # 一个只注册了 greet 的脚本：
    //   stderr: [mcp] Registered 1 tool(s) (1 enabled)
    //   tools/list 应答: {"tools":[{"name":"greet"}]}   ← 真实注册表
    // ```
    //
    // 同一件事，两个**都被当作权威**的计数，且互不引用。用户照着 13 个名字
    // 给 MCP 客户端接线，其中 `ai.create` / `ai.stream` 必然调不通
    // （本轮已从目录里删掉，全仓无实现）。
    println!("Builtin MCP tool names by toolset ({}):\n", all_tools.len());
    println!(
        "  ^ 这是**内置名字目录**，不是某个运行中服务器的工具清单。\n\
           真正对外暴露哪些工具，取决于脚本用 server.tool(name, schema, handler)\n\
           注册了什么；问那个 MCP 服务器要 tools/list，或看它启动时打印的\n\
           \"[mcp] Registered N tool(s)\"。"
    );
    println!();
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
        println!("No builtin tool names matching '{}'", query);
    } else {
        // v0.104.6 D186：与 `tool-list` 一致的口径 —— 这是**内置名字目录**的
        // 搜索结果，不是某个运行中服务器的工具清单。
        println!(
            "Builtin tool names matching '{}' ({}):\n  ^ 名字目录，非实时注册表；\
             实际对外暴露哪些工具取决于脚本注册了什么。\n",
            query,
            results.len()
        );
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

# 统计 Parser v2 和 AST v2 使用量
\ = Select-String -Path 'd:\Github\mora-lang\src' -Pattern 'ast_v2|AstArena|TypedExpr' -Recursion | Measure-Object | Select-Object -ExpandProperty Count
\ = Select-String -Path 'd:\Github\mora-lang\src' -Pattern 'parser_v2::|ParserV2' -Recursion | Measure-Object | Select-Object -ExpandProperty Count

Write-Host "AST v2 references: \"
Write-Host "Parser v2 references: \"

# 列出主要的引用文件
Select-String -Path 'd:\Github\mora-lang\src' -Pattern 'use.*ast_v2' -Recursion | Format-Table -AutoSize

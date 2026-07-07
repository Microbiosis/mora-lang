import os
import re
import sys


def strip_comments_and_strings(line):
    # Remove // comments
    line = re.sub(r"//.*", "", line)
    # Remove "..." strings (simple, no escapes)
    line = re.sub(r'"[^"]*"', '""', line)
    # Remove '...' char literals
    line = re.sub(r"'[^']*'", "''", line)
    return line


def find_test_ranges(lines):
    ranges = []
    i = 0
    while i < len(lines):
        if re.search(r"#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]", lines[i]):
            start = i
            j = i + 1
            brace_depth = 0
            found_open = False
            while j < len(lines):
                for ch in lines[j]:
                    if ch == "{":
                        brace_depth += 1
                        found_open = True
                    elif ch == "}":
                        brace_depth -= 1
                        if found_open and brace_depth == 0:
                            ranges.append((start, j))
                            i = j
                            break
                if found_open and brace_depth == 0:
                    break
                j += 1
            if not found_open:
                # module-level cfg(test) without braces: treat rest of file
                ranges.append((start, len(lines)))
                break
        i += 1
    return ranges


def in_ranges(lineno, ranges):
    for s, e in ranges:
        if s <= lineno <= e:
            return True
    return False


unwrap_re = re.compile(r"(?<![A-Za-z0-9_])\.unwrap\(\)")
panic_re = re.compile(r"(?<![A-Za-z0-9_])panic!\s*\(")
expect_re = re.compile(r"(?<![A-Za-z0-9_])\.expect\s*\(")
todo_re = re.compile(r"\b(TODO|FIXME|XXX|HACK)\b", re.IGNORECASE)


TEST_ONLY_FILES = {"record/tests.rs", "stress_tests.rs"}


def main(root):
    totals = {"unwrap": 0, "panic": 0, "expect": 0, "todo": 0}
    per_file = {}
    for dirpath, _dirs, files in os.walk(root):
        for f in files:
            if not f.endswith(".rs"):
                continue
            path = os.path.join(dirpath, f)
            rel = os.path.relpath(path, root).replace(os.sep, "/")
            if rel in TEST_ONLY_FILES:
                continue
            with open(path, "r", encoding="utf-8", errors="ignore") as fp:
                lines = fp.readlines()
            ranges = find_test_ranges(lines)
            counts = {"unwrap": 0, "panic": 0, "expect": 0, "todo": 0}
            for idx, line in enumerate(lines):
                if in_ranges(idx, ranges):
                    continue
                cleaned = strip_comments_and_strings(line)
                if unwrap_re.search(cleaned):
                    counts["unwrap"] += 1
                if panic_re.search(cleaned):
                    counts["panic"] += 1
                if expect_re.search(cleaned):
                    counts["expect"] += 1
                if todo_re.search(cleaned):
                    counts["todo"] += 1
            if any(counts.values()):
                per_file[path] = counts
                for k in totals:
                    totals[k] += counts[k]

    print(f"Total production unwrap: {totals['unwrap']}")
    print(f"Total production panic: {totals['panic']}")
    print(f"Total production expect: {totals['expect']}")
    print(f"Total production TODO/FIXME/XXX/HACK: {totals['todo']}")
    print()
    print("Per-file breakdown:")
    for path, counts in sorted(per_file.items(), key=lambda x: -sum(x[1].values())):
        rel = os.path.relpath(path, root)
        print(
            f"{rel}: unwrap={counts['unwrap']} panic={counts['panic']} "
            f"expect={counts['expect']} todo={counts['todo']}"
        )


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "src")

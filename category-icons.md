# Category Icon Pool

Use this table only when the user did not explicitly supply a default Plugin icon. Directory paths
are relative to the directory containing the loaded `plugin-creator/SKILL.md`. For the exact
manifest category, enumerate the regular `.png` files directly inside the default/light directory,
randomly choose one, and locate the file with the exact same filename in the paired dark directory.
Copy the two files into the new Plugin root as `icon.png` and `icon-dark.png`, then set
`"icon": "icon.png"` and `"darkIcon": "icon-dark.png"`.

| Manifest category      | Default/light icon directory                    | Dark icon directory                                  |
| ---------------------- | ----------------------------------------------- | ---------------------------------------------------- |
| `Office`               | `assets/category-icons/office/`                 | `assets/category-icons-dark/office/`                 |
| `Studio`               | `assets/category-icons/studio/`                 | `assets/category-icons-dark/studio/`                 |
| `Design & Sites`       | `assets/category-icons/design-and-sites/`       | `assets/category-icons-dark/design-and-sites/`       |
| `Code`                 | `assets/category-icons/code/`                   | `assets/category-icons-dark/code/`                   |
| `Business`             | `assets/category-icons/business/`               | `assets/category-icons-dark/business/`               |
| `Sales`                | `assets/category-icons/sales/`                  | `assets/category-icons-dark/sales/`                  |
| `Productivity`         | `assets/category-icons/productivity/`           | `assets/category-icons-dark/productivity/`           |
| `Science & Healthcare` | `assets/category-icons/science-and-healthcare/` | `assets/category-icons-dark/science-and-healthcare/` |
| `Education`            | `assets/category-icons/education/`              | `assets/category-icons-dark/education/`              |
| `Other`                | `assets/category-icons/other/`                  | `assets/category-icons-dark/other/`                  |

Every directory pair contains multiple choices with identical filename sets. Do not recurse into
nested directories, mix filenames within a pair, or substitute an icon from another category. If a
matching dark file is absent, report the broken built-in asset instead of creating a default-only
Plugin from this pool.

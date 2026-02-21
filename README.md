# tmignore-rs
Makes Time Machine respect .gitignore files
This tool is a drop-in replacement for [tmignore](https://github.com/samuelmeuli/tmignore). 
It will import the tmignore cache the first time it will launched.

## How to use it
```
tmignore-rs run
```
You can use `run --dry-run` to check which files will be excluded from backups.
You can get help using `--help`:
```
tmignore-rs --help
```
## Configuration
The configuration file is located at `~/.config/tmignore/config.json`.
Here is the default configuration created automatically the first time you run tmignore-rs.
```
{
  "searchPaths": [
    "~"
  ],
  "ignoredPaths": [
    "~/.Trash",
    "~/Applications",
    "~/Downloads",
    "~/Library",
    "~/Music/iTunes",
    "~/Music/Music",
    "~/Pictures/Photos Library.photoslibrary"
  ],
  "whitelist": [],
  "threads": 4
}
```
### `searchPaths`
The list of the directories to scan.

### `ignoredPaths`
The list of directories to ignore.

### `threads`
The `threads` parameter is optional, if missing the value 0 is used. 0 means the count of threads is not limited.

### `whitelist`
The `whitelist` array expects glob-style patterns:  
 - `*.broguerec` matches all files with the `.broguerec` extension
 - `*/BrogueRunHistory.txt` matches all files named `BrogueRunHistory.txt`
See https://gitlab.com/ppentchev/fnmatch-regex-rs#overview for details.
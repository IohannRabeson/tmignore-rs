# Tests

## Testing the global gitignore
To register a global gitignore:
`git config --global core.excludesFile ~/.gitignore_global`

To unregister it:
`git config --global --unset core.excludesFile`

tmignore-rs will ignore the configuration if the file does not exist.
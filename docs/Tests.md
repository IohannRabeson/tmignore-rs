# Tests

## Testing the global gitignore
To register a global gitignore:
`git config --global core.excludesFile ~/.gitignore_global`

To unregister it:
`git config --global --unset core.excludesFile`

tmignore-rs will ignore the configuration if the file does not exist.

## Testing Nix

Install Nix:
`sh <(curl --proto '=https' --tlsv1.2 -L https://nixos.org/nix/install)`

Check the file flake.nix:
`nix --extra-experimental-features 'nix-command flakes' flake check --all-systems`

Clear the cache:
`nix-collect-garbage -d`

Uninstall Nix, it's tedious:
https://nix.dev/manual/nix/2.21/installation/uninstall#macos
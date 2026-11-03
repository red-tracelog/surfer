# Surfer VS Code Extension

This extension allows you to use the [Surfer](https://surfer-project.org/) waveform viewer within VS Code.

## Features

Just install the extension and open a `.vcd`, `.fst` or `.ghw` file!

When connected to a remote VS Code workspace, the extension first tries to run
`surver` in the remote extension host so the waveform stays there. Use the
`surfer.surverPath` setting to select a compatible executable outside the
remote host's `PATH`. If Surver is unavailable, the extension still works by
forwarding the waveform through VS Code and displays a one-time performance
warning. Each waveform gets a dynamically assigned WCP endpoint. Other
workspace extensions can open a waveform and obtain that endpoint with
`vscode.commands.executeCommand<{host: string, port: number}>('surfer.wcp.openWaveform', uri)`.
The command opens or reveals that waveform and returns its WCP endpoint, allowing
the caller to attach without resetting the existing viewer state.
In a local VS Code window, the same command uses the normal direct file load
and hosts only the WCP relay in the extension process; it does not run Surver.

![Screenshot of the Surfer VS Code extension displaying a number of waveforms](https://gitlab.com/surfer-project/surfer/-/raw/main/surfer-vscode/screenshot.png)

This extension is a port of the version of Surfer that runs in a web browser, which you can try [here](https://app.surfer-project.org/). As this extension also runs Surfer in a browser, it is subject to the same restrictions as the online version - some features are missing, and it won't be as fast as the desktop version due to a lack of multithreading support.

## Known Issues

Installing this extension alongside other extensions that open `.vcd` files will likely cause trouble. If opening a `.vcd` file doesn't load Surfer, then check to make sure that no other waveform viewer extensions are installed in VS Code. If that doesn't work, then feel free to open an issue on our [Gitlab](https://gitlab.com/surfer-project/surfer)!

## Release Notes

See the main change log: https://gitlab.com/surfer-project/surfer/-/blob/main/CHANGELOG.md?ref_type=heads

### 0.3.x

This repo now automatically tracks upstream surfer.

The version of this package is derived from the upstream Surfer package version. The format is `X.Y.1zznnnnmmm` format

1. `X`: Surfer major version
2. `Y`: Surfer minor version
3. `1`: Since leading zeros are not allowed in versions
3. `z`: Surfer patch version. If the surfer version is x.y.z-dev, `Z` will be decremented by 1
4. `n`: The number of commits in the Surfer repo when this was built
4. `m`: The number of commits in this repo when this was built

With this scheme, there is a direct correspondence between upstream versions and [SemVer][semver] NPM package versions, but it does mean that the minor version will be very long



### 0.2.0
- Bumped surfer to 0.2.0 and add .fst and .ghw support to the vscode extension

### 0.1.0
- Initial Release

## Development

To update Surfer, clone the new version into `surfer`, then run `./build_extension.sh`.

To test the extension, open the `extension` dir in `VSCode` and start debugging (`F5`)

## Publishing

Run `./build_extension.sh` to update the extension surfer version and bump the version.

To publish the microsoft extension, run `vsce publish`

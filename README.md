# 🔍 yoink
**Yoink** is a powerful, Python-based TUI wrapper around `fzf`, `ripgrep`, and `bat`. It provides a seamless interface for navigating codebases, searching for files by name, or grepping for specific content, with a built-in preview and direct integration with your favorite editors (Vim, VS Code, Sublime).


# ✨ Features
- **Dual Modes:** Instantly switch between Filename Search and Content Search (grep).
- **Smart Preview:** Uses bat for syntax-highlighted previews. In content mode, it automatically scrolls to and highlights the matching line.
- **Editor Integration:** Open results directly in:
  - Vim (Ctrl+V)
  - VS Code (Ctrl+X)
  - Sublime Text (Ctrl+T)
  - System File Explorer (Ctrl+O)
  - Integrated Subshell (Enter)
- **TUI Toggles:** Toggle case sensitivity and hidden files on the fly without restarting the script.

# 📦 Dependencies
yoink requires the following binary tools to be installed and available in your system's `PATH`:
1. [**fzf**](https://github.com/junegunn/fzf): The fuzzy finder.
2. [**ripgrep (rg)**](https://github.com/BurntSushi/ripgrep): The search engine.
3. [**bat**](https://github.com/sharkdp/bat): The cat clone (used for previews).
It also requires **Python 3** and the [Rich library](https://github.com/Textualize/rich).

**Dependency Installation**

**macOS (Homebrew) [NOT OFFICIALLY SUPPORTED]:**
```
brew install fzf ripgrep bat python
```

**Ubuntu/Debian:**
```
sudo apt update
sudo apt install fzf ripgrep bat python3 python3-pip
# Note: On Ubuntu, 'bat' may be installed as 'batcat'. yoink handles this automatically.
```

**Windows (Winget) [NOT OFFICIALLY SUPPORTED]:**
```
winget install Junegunn.fzf BurntSushi.ripgrep.MSVC sharkdp.bat Python.Python.3
```

# 🚀 Installation
You can install yoink using the provided installer script. This will check for dependencies, install the Python requirements, and move the script to your local bin.

**One-Line Install (Local):**
```
chmod +x install.sh && ./install.sh
```
After installation, simply run:
```
yoink
```
# ⌨️ Usage & Keybindings
|Key|Action|
|---|------|
|Enter|Open a subshell in the file's directory (TermDir)|
|Ctrl + V|Open in Vim|
|Ctrl + X|Open in VS Code|
|Ctrl + T|Open in Sublime Text|
|Ctrl + O|Open parent folder in System Explorer|
|Ctrl + F|Switch to Filename Search Mode|
|Ctrl + G|Switch to Content Search Mode (New Search)|
|Ctrl + S|Toggle Case Sensitivity|
|Ctrl + H|Toggle Hidden Files|
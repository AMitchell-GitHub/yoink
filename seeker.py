#!/usr/bin/env python3
import os
import sys
import shutil
import subprocess
import platform

# --- Dependency Check for Python Libraries ---
try:
    from textual.app import App, ComposeResult
    from textual.containers import Container, Vertical, Horizontal
    from textual.widgets import Header, Footer, Button, RadioSet, RadioButton, Label, Switch, Select, Static, Input
    from textual.binding import Binding
    from textual import on
except ImportError:
    print("\033[91mError: Missing Python Dependency 'textual'\033[0m")
    print("This TUI requires the Textual library. Please install it:")
    print("  pip install textual")
    sys.exit(1)

# --- Configuration & Constants ---
COLORS = {
    "HEADER": "\033[95m",
    "BLUE": "\033[94m",
    "GREEN": "\033[92m",
    "WARNING": "\033[93m",
    "FAIL": "\033[91m",
    "ENDC": "\033[0m",
    "BOLD": "\033[1m",
}

BINARY_DEPENDENCIES = ["fzf", "rg", "bat"]

class SearchConfig:
    """Simple data class to hold UI configuration."""
    def __init__(self):
        self.mode = "filename"      # 'filename' or 'content'
        self.case_sensitive = False # Boolean
        self.hidden_files = True    # Boolean
        self.editor = "vim"         # 'vim', 'vscode', 'folder', 'print'
        self.initial_query = ""     # For content search

def check_dependencies():
    """Checks if fzf, rg, and bat are installed."""
    missing = []
    for dep in BINARY_DEPENDENCIES:
        if not shutil.which(dep):
            # On some Linux distros, bat is named batcat
            if dep == 'bat' and shutil.which('batcat'):
                continue
            missing.append(dep)
    
    if missing:
        print(f"{COLORS['FAIL']}Error: Missing Binary Dependencies{COLORS['ENDC']}")
        print(f"The following tools are required in PATH: {', '.join(missing)}")
        print("Please install them using your package manager (brew, apt, winget, etc.).")
        sys.exit(1)

def get_bat_command():
    """Returns 'bat' or 'batcat' depending on system."""
    if shutil.which("bat"): return "bat"
    elif shutil.which("batcat"): return "batcat"
    return "bat"

# --- TEXTUAL TUI APP ---

class SeekerTUI(App):
    """The Configuration Dashboard TUI."""
    
    CSS = """
    Screen {
        align: center middle;
    }
    
    #main_container {
        width: 60;
        height: auto;
        border: heavy $accent;
        padding: 1 2;
        background: $surface;
    }

    Label {
        margin-top: 1;
        color: $text-muted;
        text-style: bold;
    }

    .section {
        margin-bottom: 1;
    }

    #start_button {
        width: 100%;
        margin-top: 2;
        background: $success;
        color: $text;
    }
    
    #query_input {
        display: none; /* Hidden by default */
        margin-top: 1;
    }
    """

    BINDINGS = [
        ("q", "quit", "Quit"),
        ("enter", "submit", "Start Search"),
    ]

    def __init__(self):
        super().__init__()
        self.search_config = None # Will store result here

    def compose(self) -> ComposeResult:
        yield Header(show_clock=True)
        with Container(id="main_container"):
            yield Static("🔍 Seeker Configuration", id="title", classes="section")
            
            yield Label("Search Mode")
            with RadioSet(id="mode_select"):
                yield RadioButton("Filename Search", value=True, id="rb_filename")
                yield RadioButton("Content Search (Grep)", id="rb_content")
            
            # Input only shows for Content search
            yield Input(placeholder="Initial Grep Query...", id="query_input")

            yield Label("Options")
            with Vertical(classes="section"):
                with Horizontal():
                    yield Label("Case Sensitive: ", classes="switch_label")
                    yield Switch(value=False, id="sw_case")
                with Horizontal():
                    yield Label("Include Hidden: ", classes="switch_label")
                    yield Switch(value=True, id="sw_hidden")

            yield Label("Default Action (Ctrl-V/X/O override this)")
            yield Select.from_values(
                ["vim", "vscode", "folder", "print"],
                value="vim",
                id="editor_select",
                allow_blank=False
            )

            yield Button("Start Search", id="start_button", variant="success")
        yield Footer()

    @on(RadioSet.Changed, "#mode_select")
    def on_mode_change(self, event: RadioSet.Changed) -> None:
        """Toggle input box visibility based on mode."""
        input_widget = self.query_one("#query_input")
        if event.pressed.id == "rb_content":
            input_widget.styles.display = "block"
            input_widget.focus()
        else:
            input_widget.styles.display = "none"

    @on(Button.Pressed, "#start_button")
    def on_start(self) -> None:
        self.action_submit()

    def action_submit(self) -> None:
        """Gather all settings and exit the TUI."""
        config = SearchConfig()
        
        # Get Mode
        rb_content = self.query_one("#rb_content", RadioButton)
        config.mode = "content" if rb_content.value else "filename"
        
        # Get Options
        config.case_sensitive = self.query_one("#sw_case", Switch).value
        config.hidden_files = self.query_one("#sw_hidden", Switch).value
        
        # Get Editor
        editor_select = self.query_one("#editor_select", Select)
        config.editor = str(editor_select.value)
        
        # Get Query
        if config.mode == "content":
            config.initial_query = self.query_one("#query_input", Input).value

        self.search_config = config
        self.exit(result=config)

# --- LOGIC FUNCTIONS ---

def open_file(filepath, line_number=None, editor="system"):
    """Opens the file in the requested application."""
    if editor == "print":
        print(f"{filepath}:{line_number}" if line_number else filepath)
        return

    # Handle OS-specific 'Open Folder' command
    if editor == "folder":
        folder_path = os.path.dirname(os.path.abspath(filepath))
        system_platform = platform.system()
        try:
            if system_platform == "Windows":
                os.startfile(folder_path)
            elif system_platform == "Darwin":  # macOS
                subprocess.run(["open", folder_path])
            else:  # Linux
                subprocess.run(["xdg-open", folder_path])
        except Exception as e:
            print(f"{COLORS['FAIL']}Error opening folder: {e}{COLORS['ENDC']}")
        return

    # VS Code logic
    if editor == "vscode":
        cmd = ["code", "-g", f"{filepath}:{line_number}" if line_number else filepath]
        try:
            subprocess.run(cmd)
        except FileNotFoundError:
            print(f"{COLORS['FAIL']}Error: 'code' command not found.{COLORS['ENDC']}")
        return

    # Vim logic
    if editor == "vim":
        args = [filepath]
        if line_number:
            args = [f"+{line_number}", filepath]
        vim_cmd = os.getenv("EDITOR", "vim")
        try:
            subprocess.call([vim_cmd] + args)
        except FileNotFoundError:
             print(f"{COLORS['FAIL']}Error: {vim_cmd} not found.{COLORS['ENDC']}")
        return

def run_fzf(config: SearchConfig, bat_exe: str):
    """
    Constructs the complex shell command for fzf based on the config.
    """
    # 1. Construct Source Command (rg)
    if config.mode == "filename":
        # Filename search
        hidden_flag = "--hidden" if config.hidden_files else ""
        source_cmd = f"rg --files {hidden_flag} --glob '!.git/*'"
        preview_cmd = f"{bat_exe} --style=numbers --color=always {{}}"
        is_content = False
    else:
        # Content search
        hidden_flag = "--hidden" if config.hidden_files else ""
        case_flag = "--case-sensitive" if config.case_sensitive else "--smart-case"
        query = config.initial_query if config.initial_query else "."
        
        source_cmd = f"rg --line-number --no-heading --color=always {hidden_flag} {case_flag} '{query}'"
        # Preview highlights line {2}
        preview_cmd = f"{bat_exe} --style=numbers --color=always --highlight-line {{2}} {{1}}"
        is_content = True

    # 2. Key Bindings Header
    header = (
        f"ENTER: {config.editor.upper()} | "
        "CTRL-V: Vim | "
        "CTRL-X: VS Code | "
        "CTRL-O: Folder"
    )

    fzf_cmd = [
        "fzf",
        "--ansi",
        "--delimiter", ":",
        "--height", "95%",
        "--layout", "reverse",
        "--border",
        "--header", header,
        "--expect=ctrl-v,ctrl-x,ctrl-o",
        "--preview", preview_cmd,
        "--preview-window", "right:60%:wrap"
    ]

    try:
        source_proc = subprocess.Popen(source_cmd, stdout=subprocess.PIPE, shell=True)
        result = subprocess.run(
            fzf_cmd, 
            stdin=source_proc.stdout, 
            stdout=subprocess.PIPE,
            text=True
        )
        source_proc.stdout.close()
        source_proc.wait()
        return result.stdout.strip(), is_content

    except KeyboardInterrupt:
        return None, False

def parse_selection(raw_selection, is_content_search):
    if not raw_selection: return None, None
    parts = raw_selection.split(':')
    
    if is_content_search and len(parts) >= 2:
        return parts[0], parts[1] # path, line
    return raw_selection.strip(), None

def main():
    check_dependencies()
    bat_exe = get_bat_command()

    # 1. Run the TUI to get configuration
    app = SeekerTUI()
    config = app.run()

    # If user quit TUI without submitting
    if not config:
        sys.exit(0)

    # 2. Run FZF with the config
    output, is_content = run_fzf(config, bat_exe)

    if not output:
        print("No selection made.")
        sys.exit(0)

    # 3. Handle Result
    lines = output.splitlines()
    if len(lines) < 2: sys.exit(0)

    key_pressed = lines[0]
    selection_text = lines[1]
    filepath, line_num = parse_selection(selection_text, is_content)

    if not filepath: sys.exit(1)

    # 4. Determine Action
    # Keys override the default editor setting
    if key_pressed == 'ctrl-v':
        open_file(filepath, line_num, "vim")
    elif key_pressed == 'ctrl-x':
        open_file(filepath, line_num, "vscode")
    elif key_pressed == 'ctrl-o':
        open_file(filepath, line_num, "folder")
    else:
        # User pressed Enter, use the config default
        open_file(filepath, line_num, config.editor)

if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        sys.exit(0)
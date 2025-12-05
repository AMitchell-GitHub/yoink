#!/usr/bin/env python3
import os
import sys
import shutil
import subprocess
import platform

# --- Dependency Check for Python Libraries ---
try:
    from rich.console import Console
    from rich.panel import Panel
    from rich.prompt import Prompt, Confirm
    from rich import print as rprint
except ImportError:
    print("\033[91mError: Missing Python Dependency 'rich'\033[0m")
    print("This tool requires the Rich library. Please install it for Python 3:")
    print("  pip3 install rich")
    sys.exit(1)

# --- Configuration & Constants ---
BINARY_DEPENDENCIES = ["fzf", "rg", "bat"]

class SearchConfig:
    """Simple data class to hold UI configuration."""
    def __init__(self):
        self.mode = "filename"      # 'filename' or 'content'
        self.case_sensitive = False # Boolean
        self.hidden_files = False   # Boolean (Default False per request)
        self.editor = "cd"          # Default to 'cd' behavior
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
        rprint("[bold red]Error: Missing Binary Dependencies[/bold red]")
        rprint(f"The following tools are required in PATH: [yellow]{', '.join(missing)}[/yellow]")
        rprint("Please install them using your package manager (brew, apt, winget, etc.).")
        sys.exit(1)

def get_bat_command():
    """Returns 'bat' or 'batcat' depending on system."""
    if shutil.which("bat"): return "bat"
    elif shutil.which("batcat"): return "batcat"
    return "bat"

# --- LOGIC FUNCTIONS ---

def open_file(filepath, line_number=None, editor="system"):
    """Opens the file in the requested application."""
    if editor == "print":
        print(f"{filepath}:{line_number}" if line_number else filepath)
        return

    # Change Directory Mode
    # Writes the directory to a temp file for the shell wrapper to read
    if editor == "cd":
        folder_path = os.path.dirname(os.path.abspath(filepath))
        target_file = os.path.expanduser("~/.yoink_last_path")
        try:
            with open(target_file, "w") as f:
                f.write(folder_path)
            # We don't print confirmation here to avoid messing up the UI flow, 
            # the shell function will handle the jump.
        except Exception as e:
             rprint(f"[bold red]Error writing to {target_file}: {e}[/bold red]")
        return

    # Handle OS-specific 'Open Folder' command (GUI Explorer)
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
            rprint(f"[bold red]Error opening folder: {e}[/bold red]")
        return

    # VS Code logic
    if editor == "vscode":
        cmd = ["code", "-g", f"{filepath}:{line_number}" if line_number else filepath]
        try:
            subprocess.run(cmd)
        except FileNotFoundError:
            rprint("[bold red]Error: 'code' command not found.[/bold red]")
        return
        
    # Sublime Text logic
    if editor == "sublime":
        # Assumes 'subl' is in path. 
        # Sublime syntax for opening at line is `subl file:line`
        arg = f"{filepath}:{line_number}" if line_number else filepath
        cmd = ["subl", arg]
        try:
            subprocess.run(cmd)
        except FileNotFoundError:
            rprint("[bold red]Error: 'subl' command not found. Install Sublime Text CLI.[/bold red]")
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
             rprint(f"[bold red]Error: {vim_cmd} not found.[/bold red]")
        return

def run_fzf(config: SearchConfig, bat_exe: str):
    """
    Constructs the complex shell command for fzf based on the config.
    Returns: (output_string, is_content_mode_active)
    """
    # 1. Construct Source Command (rg)
    if config.mode == "filename":
        # Filename search
        hidden_flag = "--hidden" if config.hidden_files else ""
        source_cmd = f"rg --files {hidden_flag} --glob '!.git/*'"
        # Standard preview for files
        preview_cmd = f"{bat_exe} --style=numbers --color=always {{}}"
        # Standard preview window (no scroll offset)
        preview_opts = "right:60%:wrap"
        is_content = False
        prompt_str = "FILES> "
    else:
        # Content search
        hidden_flag = "--hidden" if config.hidden_files else ""
        case_flag = "--case-sensitive" if config.case_sensitive else "--smart-case"
        
        query = config.initial_query if config.initial_query else "."
        query = query.replace("'", "'\\''") # Escape quotes
        
        source_cmd = f"rg --line-number --no-heading --color=always {hidden_flag} {case_flag} '{query}'"
        # Preview highlights line {2} (file:line:content)
        preview_cmd = f"{bat_exe} --style=numbers --color=always --highlight-line {{2}} {{1}}"
        # Preview window scrolls to line {2} and centers it (-/2)
        preview_opts = "right:60%:wrap:+{2}-/2"
        is_content = True
        prompt_str = f"RG:'{config.initial_query}'> "

    # 2. Status Line Construction (With ANSI Colors for Highlighting)
    C_GREEN = "\033[1;32m"
    C_YELLOW = "\033[1;33m"
    C_DIM = "\033[2m"
    C_RESET = "\033[0m"

    mode_status = f"{C_GREEN}[FILES]{C_RESET}" if config.mode == "filename" else f"{C_GREEN}[CONTENT]{C_RESET}"
    
    # Highlight enabled options in Yellow, Dim disabled ones
    if config.case_sensitive:
        case_status = f"{C_YELLOW}[CASE:SENSITIVE]{C_RESET}"
    else:
        case_status = f"{C_DIM}[CASE:SMART]{C_RESET}"

    if config.hidden_files:
        hidden_status = f"{C_YELLOW}[HIDDEN:ON]{C_RESET}"
    else:
        hidden_status = f"{C_DIM}[HIDDEN:OFF]{C_RESET}"
    
    status_line = f"{mode_status}  {case_status}  {hidden_status}"
    
    # 3. Key Bindings Header
    # We use --expect to catch these keys and handle them in Python (restarting the loop)
    # Updated ^O label and Enter description
    controls = (
        f"ACTIONS: Enter(CD) | ^V(Vim) | ^X(Code) | ^T(Subl) | ^O(Explorer)\n"
        f"TOGGLES: ^F(Files) | ^G(New Search) | ^S(Case) | ^H(Hidden)"
    )

    fzf_cmd = [
        "fzf",
        "--ansi",
        "--delimiter", ":",
        "--height", "95%",
        "--layout", "reverse",
        "--border",
        "--prompt", prompt_str,
        "--header", f"{status_line}\n{controls}",
        # Bindings to return specific keys to Python
        "--expect=ctrl-v,ctrl-x,ctrl-o,ctrl-t,ctrl-f,ctrl-g,ctrl-s,ctrl-h",
        "--preview", preview_cmd,
        "--preview-window", preview_opts
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
        return result.stdout, is_content

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
    console = Console()
    config = SearchConfig()

    # Clear previous last path file if it exists so we don't accidentally CD on cancel
    last_path_file = os.path.expanduser("~/.yoink_last_path")
    if os.path.exists(last_path_file):
        try:
            os.remove(last_path_file)
        except OSError:
            pass

    # --- Main Event Loop ---
    # This loop keeps running fzf until an actual file action is taken
    # or the user cancels.
    while True:
        # Prompt for query if in content mode and no query set
        if config.mode == "content" and not config.initial_query:
             console.clear()
             rprint(Panel(f"[bold cyan]Content Search Mode[/bold cyan]", subtitle="Enter text to search for (ripgrep)"))
             q = Prompt.ask("[bold green]Search Query[/bold green]")
             if not q:
                 q = "."
             config.initial_query = q

        output, is_content = run_fzf(config, bat_exe)

        if not output:
            sys.exit(0)

        lines = output.splitlines()
        if len(lines) < 2:
            if len(lines) == 1:
                key_pressed = lines[0]
                selection_text = None
            else:
                sys.exit(0)
        else:
            key_pressed = lines[0]
            selection_text = lines[1]

        # --- Handle Toggle/Switch Keys (Loop Continue) ---
        if key_pressed == 'ctrl-f':
            config.mode = "filename"
            continue # Restart loop with new mode
            
        elif key_pressed == 'ctrl-g':
            config.mode = "content"
            config.initial_query = "" 
            continue 

        elif key_pressed == 'ctrl-s':
            config.case_sensitive = not config.case_sensitive
            continue

        elif key_pressed == 'ctrl-h':
            config.hidden_files = not config.hidden_files
            continue

        # --- Handle Action Keys (Loop Break) ---
        if not selection_text:
            continue

        filepath, line_num = parse_selection(selection_text, is_content)
        if not filepath: 
            sys.exit(1)

        if key_pressed == 'ctrl-v':
            open_file(filepath, line_num, "vim")
            break
        elif key_pressed == 'ctrl-x':
            open_file(filepath, line_num, "vscode")
            break
        elif key_pressed == 'ctrl-t':
            open_file(filepath, line_num, "sublime")
            break
        elif key_pressed == 'ctrl-o':
            open_file(filepath, line_num, "folder")
            break
        else:
            # Default Enter key (CD)
            open_file(filepath, line_num, config.editor)
            break

if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        sys.exit(0)
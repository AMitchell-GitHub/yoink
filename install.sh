#!/bin/bash

# Configuration
SCRIPT_NAME="yoink.py"
INSTALL_NAME="yoink"
INSTALL_DIR="$HOME/.local/bin"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

echo -e "${BLUE}=== Yoink Installer ===${NC}"

# 1. Check for Python 3
if ! command -v python3 &> /dev/null; then
    echo -e "${RED}Error: Python 3 is not installed.${NC}"
    exit 1
fi
echo -e "${GREEN}✓ Python 3 found${NC}"

# 2. Check for Pip (via module)
if ! python3 -m pip --version &> /dev/null; then
    echo -e "${YELLOW}Warning: 'python3 -m pip' failed. You might need to install python3-pip.${NC}"
fi

# 3. Check Binary Dependencies (Warning only, does not stop install)
DEPENDENCIES=("fzf" "rg")
MISSING_DEPS=0

for dep in "${DEPENDENCIES[@]}"; do
    if ! command -v $dep &> /dev/null; then
        echo -e "${YELLOW}Warning: '$dep' is not installed or not in PATH.${NC}"
        MISSING_DEPS=1
    else
        echo -e "${GREEN}✓ $dep found${NC}"
    fi
done

# Special check for bat vs batcat
if command -v bat &> /dev/null; then
    echo -e "${GREEN}✓ bat found${NC}"
elif command -v batcat &> /dev/null; then
    echo -e "${GREEN}✓ batcat found (Ubuntu alias)${NC}"
else
    echo -e "${YELLOW}Warning: 'bat' (or 'batcat') is not installed.${NC}"
    MISSING_DEPS=1
fi

if [ $MISSING_DEPS -eq 1 ]; then
    echo -e "${YELLOW}Some binary dependencies are missing. Please install them manually (see README.md).${NC}"
    sleep 2
fi

# 4. Install Python 'rich' library
# We use 'python3 -m pip' to avoid 'bad interpreter' errors with broken pip3 wrappers
echo -e "${BLUE}Installing Python 'rich' library...${NC}"
if python3 -m pip install rich --user; then
    echo -e "${GREEN}✓ rich installed${NC}"
else
    echo -e "${RED}Error: Failed to install 'rich'. Check your python/pip setup.${NC}"
    exit 1
fi

# 5. Install Script
if [ ! -f "$SCRIPT_NAME" ]; then
    echo -e "${RED}Error: $SCRIPT_NAME not found in current directory.${NC}"
    exit 1
fi

# Create directory if it doesn't exist
mkdir -p "$INSTALL_DIR"

echo -e "${BLUE}Installing to $INSTALL_DIR/$INSTALL_NAME...${NC}"
cp "$SCRIPT_NAME" "$INSTALL_DIR/$INSTALL_NAME"
chmod +x "$INSTALL_DIR/$INSTALL_NAME"

# 6. Check PATH
if [[ ":$PATH:" != *":$INSTALL_DIR:"* ]]; then
    echo -e "${YELLOW}Warning: $INSTALL_DIR is not in your PATH.${NC}"
    echo "Add the following line to your .bashrc or .zshrc:"
    echo "  export PATH=\"\$HOME/.local/bin:\$PATH\""
fi

echo -e "${GREEN}Success! Yoink is installed.${NC}"
echo -e "You can now run it by typing: ${BLUE}$INSTALL_NAME${NC}"

echo -e "\n${YELLOW}IMPORTANT: To enable 'CD on Enter' functionality:${NC}"
echo -e "Copy the following function into your ${BLUE}~/.bashrc${NC} or ${BLUE}~/.zshrc${NC}:"
echo -e "${GREEN}"
echo "yoink() {"
echo "    command yoink \"\$@\""
echo "    local dest=\"\$HOME/.yoink_last_path\""
echo "    if [ -f \"\$dest\" ]; then"
echo "        cd \"\$(cat \"\$dest\")\""
echo "        rm \"\$dest\""
echo "    fi"
echo "}"
echo -e "${NC}"
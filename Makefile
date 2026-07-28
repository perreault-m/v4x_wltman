# ============================================================
# Configuration
# ============================================================
APP_NAME       = V4X Wallet
BINARY_NAME    = gui
ICON_SRC       = assets/Icon.png
DESKTOP_ID     = v4x-wallet
COMMENT        = V4X Community Wallet manager
CATEGORIES     = Utility;Development;

# Paths
RELEASE_DIR    = target/release
RELEASE_BIN    = $(RELEASE_DIR)/$(BINARY_NAME)

# Icon theme location (this is the important part)
ICON_THEME_DIR = $(HOME)/.local/share/icons/hicolor/256x256/apps
ICON_DEST      = $(ICON_THEME_DIR)/$(DESKTOP_ID).png

# .desktop stays next to the binary (or you can put it in applications/)
DESKTOP_FILE   = $(RELEASE_DIR)/$(DESKTOP_ID).desktop

# ============================================================
# Linux target
# ============================================================
linux: $(RELEASE_BIN)
	@echo "→ Installing icon into icon theme + generating .desktop file..."
	@mkdir -p $(ICON_THEME_DIR) $(RELEASE_DIR)
	@cp $(ICON_SRC) $(ICON_DEST)
	@cp $(ICON_SRC) $(RELEASE_DIR)/$(DESKTOP_ID).png   # also keep a copy next to binary
	@echo "[Desktop Entry]"                        >  $(DESKTOP_FILE)
	@echo "Version=1.0"                            >> $(DESKTOP_FILE)
	@echo "Type=Application"                       >> $(DESKTOP_FILE)
	@echo "Name=$(APP_NAME)"                       >> $(DESKTOP_FILE)
	@echo "Comment=$(COMMENT)"                     >> $(DESKTOP_FILE)
	@echo "Exec=$$(realpath $(RELEASE_BIN)) %F"    >> $(DESKTOP_FILE)
	@echo "Icon=$(DESKTOP_ID)"                     >> $(DESKTOP_FILE)
	@echo "Terminal=false"                         >> $(DESKTOP_FILE)
	@echo "Categories=$(CATEGORIES)"               >> $(DESKTOP_FILE)
	@echo "StartupNotify=true"                     >> $(DESKTOP_FILE)
	@chmod +x $(DESKTOP_FILE)
	@gtk-update-icon-cache -f -t $(HOME)/.local/share/icons/hicolor 2>/dev/null || true
	@echo "✓ Done!"
	@echo "  Icon installed as: $(ICON_DEST)"
	@echo "  Desktop file:      $(DESKTOP_FILE)"

$(RELEASE_BIN):
	cargo build --release
BINARY    := downloadstzset
BIN_DIR   := $(HOME)/.local/bin
PLIST_ID  := me.chelseau.downloadstzset
PLIST_SRC := $(PLIST_ID).plist.in
PLIST_DST := $(HOME)/Library/LaunchAgents/$(PLIST_ID).plist
LOG_DIR   := $(HOME)/Library/Logs
UID       := $(shell id -u)

.PHONY: build run install uninstall

build:
	cargo build --release

run:
	cargo run --release

install: build
	mkdir -p $(BIN_DIR)
	install -m 755 target/release/$(BINARY) $(BIN_DIR)/$(BINARY)
	mkdir -p $(LOG_DIR)
	sed \
	  -e 's|@@BINARY@@|$(BIN_DIR)/$(BINARY)|g' \
	  -e 's|@@LOG_DIR@@|$(LOG_DIR)|g' \
	  $(PLIST_SRC) > $(PLIST_DST)
	-launchctl bootout gui/$(UID) $(PLIST_DST) 2>/dev/null; true
	launchctl bootstrap gui/$(UID) $(PLIST_DST)
	@echo "Installed and started. Logs: $(LOG_DIR)/$(BINARY).log"

uninstall:
	-launchctl bootout gui/$(UID) $(PLIST_DST)
	rm -f $(PLIST_DST)
	rm -f $(BIN_DIR)/$(BINARY)
	@echo "Uninstalled."

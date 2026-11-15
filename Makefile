APP_NAME := stt-md
BIN_NAME := stt-md
BUILD_DIR := dist
APP_BUNDLE := $(BUILD_DIR)/$(APP_NAME).app
TARGET_DIR := target/release
# macOS ties privacy grants (Screen Recording for system audio, mic) to the
# code signature. Ad-hoc signatures change on every build, so each reinstall
# silently revoked the grant and recordings fell back to mic-only. Sign with
# the first Apple Development identity when there is one; override with
# SIGN_IDENTITY=... or fall back to ad-hoc ("-").
SIGN_IDENTITY ?= $(shell security find-identity -v -p codesigning 2>/dev/null | awk -F'"' '/Apple Development/ {print $$2; exit}')

.PHONY: dev build install run clean check test $(TARGET_DIR)/$(BIN_NAME)

dev:
	cargo run

check:
	cargo check

test:
	cargo test

build: $(APP_BUNDLE)

$(APP_BUNDLE): $(TARGET_DIR)/$(BIN_NAME) Info.plist assets/AppIcon.icns
	@mkdir -p $(APP_BUNDLE)/Contents/MacOS
	@mkdir -p $(APP_BUNDLE)/Contents/Resources
	@cp $(TARGET_DIR)/$(BIN_NAME) $(APP_BUNDLE)/Contents/MacOS/$(BIN_NAME)
	@cp Info.plist $(APP_BUNDLE)/Contents/Info.plist
	@cp assets/AppIcon.icns $(APP_BUNDLE)/Contents/Resources/AppIcon.icns
	@codesign --force --sign "$(or $(SIGN_IDENTITY),-)" $(APP_BUNDLE)
	@touch $(APP_BUNDLE)
	@echo "Built $(APP_BUNDLE)"

# PHONY on purpose: cargo decides if a rebuild is needed. A file target here
# made `make build` skip recompilation after source changes (stale bundles).
$(TARGET_DIR)/$(BIN_NAME):
	cargo build --release

# The bundle in dist/ is a build artifact, but Spotlight indexes it next to
# the installed copy and both answer to "stt-md". Install moves it out of the
# way so only /Applications/stt-md.app is left to launch. The running app is
# asked to quit first (pkill is for versions that predate `quit`), so a
# recording or a note in progress survives the swap: it stays in the queue
# and the relaunched app picks it up.
install: build
	@/Applications/$(APP_NAME).app/Contents/MacOS/$(BIN_NAME) quit >/dev/null 2>&1; sleep 1; pkill -x $(BIN_NAME) || true
	@rm -rf /Applications/$(APP_NAME).app
	@ditto $(APP_BUNDLE) /Applications/$(APP_NAME).app
	@rm -rf $(APP_BUNDLE)
	@open /Applications/$(APP_NAME).app
	@echo "Installed and relaunched /Applications/$(APP_NAME).app"

run: build
	open $(APP_BUNDLE)

clean:
	cargo clean
	rm -rf $(BUILD_DIR)

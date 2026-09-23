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

.PHONY: dev build run clean check test $(TARGET_DIR)/$(BIN_NAME)

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

run: build
	open $(APP_BUNDLE)

clean:
	cargo clean
	rm -rf $(BUILD_DIR)

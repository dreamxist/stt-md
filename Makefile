APP_NAME := stt-md
BIN_NAME := stt-md
BUILD_DIR := dist
APP_BUNDLE := $(BUILD_DIR)/$(APP_NAME).app
TARGET_DIR := target/release

.PHONY: dev build run clean check

dev:
	cargo run

check:
	cargo check

build: $(APP_BUNDLE)

$(APP_BUNDLE): $(TARGET_DIR)/$(BIN_NAME) Info.plist
	@mkdir -p $(APP_BUNDLE)/Contents/MacOS
	@mkdir -p $(APP_BUNDLE)/Contents/Resources
	@cp $(TARGET_DIR)/$(BIN_NAME) $(APP_BUNDLE)/Contents/MacOS/$(BIN_NAME)
	@cp Info.plist $(APP_BUNDLE)/Contents/Info.plist
	@touch $(APP_BUNDLE)
	@echo "Built $(APP_BUNDLE)"

$(TARGET_DIR)/$(BIN_NAME):
	cargo build --release

run: build
	open $(APP_BUNDLE)

clean:
	cargo clean
	rm -rf $(BUILD_DIR)

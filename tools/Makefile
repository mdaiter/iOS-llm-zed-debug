PROJECT ?= /path/to/YourApp.xcodeproj
SCHEME ?= YourApp
PROGRAM ?= ./target/debug/ios-lldb-dap
PORT ?= 0

.PHONY: host-test
host-test:
	cargo test

.PHONY: zed-host-demo
zed-host-demo:
	cargo run --bin ios-lldb-gendebug -- --program $(PROGRAM) --port $(PORT) --label "Host (ios-lldb)" --write
	@echo "Open Zed and pick the newly written configuration in .zed/debug.json"

.PHONY: ios-sim-demo
ios-sim-demo:
	cargo run --bin ios-lldb-setup -- --mode sim --project $(PROJECT) --scheme $(SCHEME) --write --wait

.PHONY: ios-device-demo
ios-device-demo:
	cargo run --bin ios-lldb-setup -- --mode device --project $(PROJECT) --scheme $(SCHEME) --write --wait

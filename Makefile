# scrnsav — build, run, and install targets.
# Everything goes through cargo; this is a convenience layer.

CARGO    ?= cargo
BIN       = scrnsav
RELEASE   = target/release/$(BIN)
DEBUG     = target/debug/$(BIN)

# Idle timeout (seconds) used by the `watch` / `run` targets.
IDLE     ?= 300

# Optional bundled name, WGSL file path, or random. Empty = bundled default effect.
#   make run SHADER=random
SHADER   ?=
SHADER_ARG = $(if $(SHADER),--shader $(SHADER),)

# Size of the generated README screenshots (override: make shots SHOT_SIZE=1920x1080).
SHOT_SIZE ?= 960x540
# Extra flags for the shot targets, e.g. make shots SHOT_ARGS="--seed 2.0"
SHOT_ARGS ?=

# Where the systemd user unit gets installed.
UNIT_DIR  = $(HOME)/.config/systemd/user
UNIT      = scrnsav.service

.DEFAULT_GOAL := build

## build: optimized release binary (what you install)
.PHONY: build
build:
	$(CARGO) build --release

## debug: unoptimized build with debug info
.PHONY: debug
debug:
	$(CARGO) build

## run: build release, then run the saver fullscreen now (any input exits)
.PHONY: run
run: build
	$(RELEASE) show $(SHADER_ARG)

## run-all: Show each of the available shaders
.PHONY: run-all
run-all: build
	for s in shaders/*.wgsl; do \
	  $(RELEASE) show --shader $$s; \
	done

## shots: render a PNG of each shader to docs/shots/ (headless, no display)
.PHONY: shots
shots: build
	mkdir -p docs/shots
	for s in shaders/*.wgsl; do \
	  for i in 1 2; do \
	    $(RELEASE) shot --shader $$s --size $(SHOT_SIZE) \
	      --out docs/shots/$$(basename $$s .wgsl)$$i.png $(SHOT_ARGS); \
	  done; \
	done

## ci-shots: render every shader to a throwaway dir (GPU smoke test, no tracked files touched)
# Needs a working GPU, so `make ci` does too. Fine locally; a GitHub Actions
# runner has none — install Mesa's software renderer (lavapipe/llvmpipe) there
# first, or drop ci-shots from the `ci` deps and run it as its own job.
.PHONY: ci-shots
ci-shots: build
	@tmp=$$(mktemp -d); \
	trap 'rm -rf "$$tmp"' EXIT; \
	for s in shaders/*.wgsl; do \
	  $(RELEASE) shot --shader $$s --size $(SHOT_SIZE) \
	    --out "$$tmp/$$(basename $$s .wgsl).png" || exit 1; \
	done; \
	echo "ci-shots: rendered $$(ls "$$tmp" | wc -l) shader(s) OK (discarded)"

## watch: build release, then run the idle daemon (IDLE=<secs>, with logging)
.PHONY: watch
watch: build
	RUST_LOG=info $(RELEASE) watch $(IDLE) $(SHADER_ARG)

## check: fast type-check without producing a binary
.PHONY: check
check:
	$(CARGO) check

## test: run tests (includes validating every shader in shaders/)
.PHONY: test
test:
	$(CARGO) test

## fmt: format the source
.PHONY: fmt
fmt:
	$(CARGO) fmt

## fmt-check: fail if the source isn't formatted (for CI)
.PHONY: fmt-check
fmt-check:
	$(CARGO) fmt --check

## ci: Run all checks that might block a merge
.PHONY: ci
ci: CLIPPY_FLAGS = -- -D warnings
ci: fmt-check build test clippy ci-shots

## clippy: lint (CI promotes warnings to errors via CLIPPY_FLAGS)
.PHONY: clippy
clippy:
	$(CARGO) clippy --all-targets $(CLIPPY_FLAGS)

## install: build + install and enable the service (IDLE=<secs>, SHADER=<name|path|random>)
.PHONY: install
install: build
	mkdir -p $(UNIT_DIR)
	sed 's|^ExecStart=.*|ExecStart=$(CURDIR)/$(RELEASE) watch $(IDLE) $(SHADER_ARG)|' \
		$(UNIT) > $(UNIT_DIR)/$(UNIT)
	systemctl --user daemon-reload
	systemctl --user enable --now $(UNIT)
	@echo "Installed: watch $(IDLE)s. Tip: disable GNOME's own blank so scrnsav wins:"
	@echo "  gsettings set org.gnome.desktop.session idle-delay 0"

## uninstall: stop and remove the systemd --user service
.PHONY: uninstall
uninstall:
	-systemctl --user disable --now $(UNIT)
	-rm -f $(UNIT_DIR)/$(UNIT)
	systemctl --user daemon-reload

## clean: remove build artifacts
.PHONY: clean
clean:
	$(CARGO) clean

## help: list targets
.PHONY: help
help:
	@grep -E '^## ' $(MAKEFILE_LIST) | sed 's/## /  /'

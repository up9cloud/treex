# `make dev` is the npm-run-dev equivalent: TUI and web view together, on this
# repo. Everything else is a parameter.
#
#   make dev                              TUI + web on 127.0.0.1:11711
#   make dev DIR=~/src                    somewhere else
#   make dev WEB=0                        TUI only
#   make dev TUI=0 HOST=0.0.0.0           headless, reachable on the LAN
#   make dev WATCH=0 PORT=9000

DIR      ?= .
HOST     ?= 127.0.0.1
PORT     ?= 11711
TUI      ?= 1
WEB      ?= 1
WATCH    ?= 1
# Empty means the default features, which is what a stock install gets.
FEATURES ?=
CARGO    ?= cargo

ARGS := $(DIR)
ifeq ($(WEB),1)
  ARGS += --web $(HOST):$(PORT)
endif
ifeq ($(TUI),0)
  ARGS += --no-tui
endif
ifeq ($(WATCH),0)
  ARGS += --no-watch
endif

.PHONY: help dev print watch test page-test names lint fmt check-all doc release install clean

help:
	@grep -E '^[a-z-]+:.*## ' $(MAKEFILE_LIST) | sed 's/:.*## /\t/' | expand -t14
	@echo
	@echo "make dev parameters (current values):"
	@echo "  DIR=$(DIR)  HOST=$(HOST)  PORT=$(PORT)"
	@echo "  TUI=$(TUI)  WEB=$(WEB)  WATCH=$(WATCH)"
	@echo "  FEATURES=$(FEATURES)   (empty means the default features)"
	@echo
	@echo "HOST=127.0.0.1 means the web view is unreachable from other machines."
	@echo "To read the tree from a phone or over a VPN:"
	@echo "  make dev HOST=0.0.0.0"

dev: ## Run treex: TUI + web by default, see parameters below
	$(CARGO) run $(FEATURES) -- $(ARGS)

print: ## Print the tree and exit
	$(CARGO) run $(FEATURES) -- --print -L 3 $(DIR)

watch: ## Rebuild and relaunch on change (cargo install bacon)
	bacon run -- $(FEATURES) -- $(ARGS)

test: ## Run every test
	$(CARGO) test --all-features

page-test: ## Drive the real web page against a real server (needs node)
	$(CARGO) build --all-features --quiet
	node tool/page-test.mjs

names: ## Check every path can be checked out on Windows
	node tool/check-filenames.mjs

lint: ## What CI's lint job runs
	$(CARGO) fmt --all --check
	RUSTFLAGS="-D warnings" $(CARGO) clippy --all-features --all-targets
	node tool/check-filenames.mjs

# Every feature is advertised as optional, so each has to build alone.
check-all: ## Build each feature combination, as CI does
	@for f in "" tui web watch tui,web tui,watch web,watch tui,web,watch; do \
		printf '%-16s ' "[$$f]"; \
		if RUSTFLAGS="-D warnings" $(CARGO) check --no-default-features --features "$$f" --quiet 2>/dev/null; \
			then echo OK; else echo FAIL; fi; \
	done

doc: ## Build and open the library documentation
	$(CARGO) doc --all-features --no-deps --open

release: ## Optimized build
	$(CARGO) build --release --all-features

install: ## Install from this checkout
	$(CARGO) install --path . --all-features

clean:
	$(CARGO) clean

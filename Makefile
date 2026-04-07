SHELL=/bin/bash
.DEFAULT_GOAL=_help

PREFIX ?= $(HOME)/.local
NPROC_CMD := $(shell nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 4)
UNAME_S := $(shell uname -s)


.PHONY: prebuild
prebuild: ## Build RocksDB shared library, static library, and ldb binary locally
	@echo "Building RocksDB shared library, static library, and ldb binary..."
	@if [ ! -f "librocksdb-sys/rocksdb/Makefile" ]; then \
		echo "RocksDB submodule not found. Initializing..."; \
		git submodule update --init --recursive; \
	fi
	cd librocksdb-sys/rocksdb && env ROCKSDB_NO_FBCODE=1 DISABLE_JEMALLOC=1 \
		EXTRA_CXXFLAGS="$${EXTRA_CXXFLAGS:-} -I$(PREFIX)/include -Wno-error=unused-parameter" \
		EXTRA_LDFLAGS="$${EXTRA_LDFLAGS:-} -L$(PREFIX)/lib" PORTABLE=0 USE_RTTI=1 \
		make shared_lib static_lib -j$(NPROC_CMD)
	cd librocksdb-sys/rocksdb && env DISABLE_WARNING_AS_ERROR=1 DEBUG_LEVEL=0 USE_RTTI=1 make ldb
	@echo ""
	@echo "Prebuild complete! Run 'make install' (or 'sudo make install PREFIX=/usr/local/zaidoon') to install natively."

.PHONY: install
install: prebuild ## Install RocksDB to the configured PREFIX
	@echo "Installing RocksDB to $(PREFIX)..."
	cd librocksdb-sys/rocksdb && make install-shared INSTALL_PATH=$(PREFIX)
	cd librocksdb-sys/rocksdb && make install-static INSTALL_PATH=$(PREFIX)
	mkdir -p $(PREFIX)/bin
	cp -p librocksdb-sys/rocksdb/ldb $(PREFIX)/bin/ldb
	@if [ "$(UNAME_S)" = "Linux" ]; then \
		ldconfig $(PREFIX)/lib 2>/dev/null || true; \
	fi
	@echo "============================================================"
	@echo "Installation complete!"
	@echo "To use these cached libraries and bypass Cargo recompiling"
	@echo "the C++ dependencies, export the following in your shell:"
	@echo ""
	@echo "    export PATH=\"\$$PATH:$(PREFIX)/bin\""
	@echo ""
	@echo "A Cargo override config needs to be created in .cargo/config.toml."
	@echo "Please see CONTRIBUTING.md for the suggested configuration to link"
	@echo "Cargo to natively built libraries."
	@echo "============================================================"

.PHONY: clean
clean: ## Clean upstream RocksDB build artifacts
	cd librocksdb-sys/rocksdb && make clean

# [ENUM] Styling / Colors
STYLE_CYAN := $(shell tput setaf 6 2>/dev/null || echo '\033[36m')
STYLE_RESET := $(shell tput sgr0 2>/dev/null || echo '\033[0m')

# List available commands
.PHONY: _help
_help:
	@echo "Available commands:"
	@grep -hE '^[a-zA-Z0-9_-]+:.*?## .*$$' $(MAKEFILE_LIST) \
		| awk 'BEGIN {FS = ":.*?## "}; {printf "  $(STYLE_CYAN)%-15s$(STYLE_RESET) %s\n", $$1, $$2}'

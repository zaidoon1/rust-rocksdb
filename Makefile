SHELL=/bin/bash
.DEFAULT_GOAL=_help

PREFIX ?= $(HOME)/.local
# Disable jemalloc by default for RocksDB build, but can be overridden from command line or env
DISABLE_JEMALLOC ?= 1
NPROC_CMD := $(shell nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 4)
UNAME_S := $(shell uname -s)


# Auto-detect optimal compiler toolchain
HAS_CLANG := $(shell command -v clang++ 2>/dev/null)
HAS_MOLD := $(shell command -v mold 2>/dev/null)
HAS_SCCACHE := $(shell command -v sccache 2>/dev/null)

CXX_COMPILER ?= $(if $(HAS_CLANG),clang++,g++)
CC_COMPILER ?= $(if $(HAS_CLANG),clang,gcc)

ifneq ($(HAS_SCCACHE),)
  export USE_CCACHE=0
  CXX_CMD ?= sccache $(CXX_COMPILER)
  CC_CMD ?= sccache $(CC_COMPILER)
else
  CXX_CMD ?= $(CXX_COMPILER)
  CC_CMD ?= $(CC_COMPILER)
endif

UNAME_M := $(shell uname -m)
MOLD_LDFLAG := $(if $(filter x86_64,$(UNAME_M)),$(if $(HAS_MOLD),-fuse-ld=mold,),)


.PHONY: bootstrap
bootstrap: ## Install high-performance build tools and setup Cargo config
	@echo "Bootstrapping development environment (requires sudo)..."
	@if command -v apt-get >/dev/null 2>&1; then \
		sudo apt-get update && sudo apt-get install -y clang mold sccache ccache make llvm libclang-dev cmake ninja-build libsnappy-dev liblz4-dev libzstd-dev zlib1g-dev libbz2-dev; \
	elif command -v pacman >/dev/null 2>&1; then \
		sudo pacman -Sy --needed clang mold sccache ccache make llvm cmake ninja snappy lz4 zstd zlib bzip2; \
	elif command -v dnf >/dev/null 2>&1; then \
		sudo dnf install -y clang mold sccache ccache make llvm cmake ninja-build snappy-devel lz4-devel libzstd-devel zlib-devel bzip2-devel; \
	elif command -v brew >/dev/null 2>&1; then \
		brew install llvm sccache ccache make cmake ninja snappy lz4 zstd zlib bzip2; \
	else \
		echo "Unsupported package manager. Please install clang, mold, and sccache manually."; \
	fi
	@echo "Bootstrapping local Cargo caching configuration..."
	@mkdir -p .cargo
	@if [ ! -f .cargo/config.toml ]; then \
		if command -v sccache >/dev/null 2>&1; then \
			echo "[build]" > .cargo/config.toml; \
			echo "rustc-wrapper = \"sccache\"" >> .cargo/config.toml; \
			echo "" >> .cargo/config.toml; \
		fi; \
		echo "[env]" >> .cargo/config.toml; \
		echo "ROCKSDB_LIB_DIR = \"$(PREFIX)/lib\"" >> .cargo/config.toml; \
		echo "ROCKSDB_INCLUDE_DIR = \"$(PREFIX)/include\"" >> .cargo/config.toml; \
		echo "PKG_CONFIG_PATH = { value = \"$(PREFIX)/lib/pkgconfig\", force = false }" >> .cargo/config.toml; \
		echo "ROCKSDB_STATIC = \"1\"" >> .cargo/config.toml; \
		echo "LD_LIBRARY_PATH = { value = \"$(PREFIX)/lib\", force = false }" >> .cargo/config.toml; \
		if command -v sccache >/dev/null 2>&1 && command -v clang++ >/dev/null 2>&1; then \
			echo "CC = \"sccache clang\"" >> .cargo/config.toml; \
			echo "CXX = \"sccache clang++\"" >> .cargo/config.toml; \
		elif command -v sccache >/dev/null 2>&1; then \
			echo "CC = \"sccache gcc\"" >> .cargo/config.toml; \
			echo "CXX = \"sccache g++\"" >> .cargo/config.toml; \
		elif command -v clang++ >/dev/null 2>&1; then \
			echo "CC = \"clang\"" >> .cargo/config.toml; \
			echo "CXX = \"clang++\"" >> .cargo/config.toml; \
		fi; \
		if command -v mold >/dev/null 2>&1; then \
			echo "" >> .cargo/config.toml; \
			echo "[target.x86_64-unknown-linux-gnu]" >> .cargo/config.toml; \
			echo "rustflags = [\"-C\", \"link-arg=-fuse-ld=mold\"]" >> .cargo/config.toml; \
			echo "" >> .cargo/config.toml; \
			echo "[target.aarch64-unknown-linux-gnu]" >> .cargo/config.toml; \
			echo "rustflags = [\"-C\", \"link-arg=-fuse-ld=mold\"]" >> .cargo/config.toml; \
		fi; \
		echo "Generated .cargo/config.toml pointing to $(PREFIX)."; \
	else \
		echo ".cargo/config.toml already exists, skipping generation."; \
	fi
	@echo "Bootstrap complete! Run 'make prebuild' to build with these optimized tools."

.PHONY: prebuild
prebuild: ## Build RocksDB shared library, static library, and ldb binary locally
	@echo "Building RocksDB shared library, static library, and ldb binary using $(CXX_CMD)..."
	@if [ ! -f "librocksdb-sys/rocksdb/Makefile" ]; then \
		echo "RocksDB submodule not found. Initializing..."; \
		git submodule update --init --recursive; \
	fi
	@mkdir -p $(PREFIX)/include $(PREFIX)/lib
	cd librocksdb-sys/rocksdb && \
		env ROCKSDB_NO_FBCODE=1 DISABLE_JEMALLOC=$(DISABLE_JEMALLOC) ROCKSDB_DISABLE_BENCHMARK=1 CC="$(CC_CMD)" CXX="$(CXX_CMD)" \
		EXTRA_CXXFLAGS="$${EXTRA_CXXFLAGS:-} -I$(PREFIX)/include -Wno-error=unused-parameter" \
		EXTRA_LDFLAGS="$${EXTRA_LDFLAGS:-} $(MOLD_LDFLAG) -L$(PREFIX)/lib" PORTABLE=0 USE_RTTI=1 \
		make shared_lib static_lib -j$(NPROC_CMD)
	cd librocksdb-sys/rocksdb && \
		env DISABLE_WARNING_AS_ERROR=1 ROCKSDB_DISABLE_BENCHMARK=1 DEBUG_LEVEL=0 USE_RTTI=1 CC="$(CC_CMD)" CXX="$(CXX_CMD)" make ldb
	@echo ""
	@echo "Prebuild complete! Run 'make install' (or 'sudo make install PREFIX=/usr/local/rust-rocksdb') to install natively."


.PHONY: install
install: prebuild ## Install built RocksDB to the configured PREFIX
	@echo "Installing RocksDB to $(PREFIX)..."
	cd librocksdb-sys/rocksdb && make install-shared INSTALL_PATH=$(PREFIX)
	cd librocksdb-sys/rocksdb && make install-static INSTALL_PATH=$(PREFIX)
	mkdir -p $(PREFIX)/bin
	cp -p librocksdb-sys/rocksdb/ldb $(PREFIX)/bin/ldb
	@if [ "$(UNAME_S)" = "Darwin" ]; then \
		if command -v install_name_tool >/dev/null 2>&1; then \
			install_name_tool -id "$(PREFIX)/lib/librocksdb.11.dylib" "$(PREFIX)/lib/librocksdb.11.dylib" 2>/dev/null || true; \
			install_name_tool -id "$(PREFIX)/lib/librocksdb.11.0.dylib" "$(PREFIX)/lib/librocksdb.11.0.dylib" 2>/dev/null || true; \
			install_name_tool -id "$(PREFIX)/lib/librocksdb.dylib" "$(PREFIX)/lib/librocksdb.dylib" 2>/dev/null || true; \
			echo "Updated library install names to absolute paths for macOS dyld compatibility."; \
		fi; \
	fi
	@if [ "$(UNAME_S)" = "Linux" ]; then \
		if [ -w "$(PREFIX)/lib" ] && [[ ! "$(PREFIX)" =~ ^"$(HOME)" ]]; then \
			ldconfig $(PREFIX)/lib 2>/dev/null || echo "Warning: ldconfig failed. You may need to run: sudo ldconfig $(PREFIX)/lib" >&2; \
		else \
			if [[ "$(PREFIX)" =~ ^"$(HOME)" ]]; then \
				echo "Skipping ldconfig for non-system installation path under HOME."; \
			else \
				echo "Warning: $(PREFIX)/lib is not writable. Skipping ldconfig update. Please run: sudo ldconfig $(PREFIX)/lib" >&2; \
			fi; \
		fi; \
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

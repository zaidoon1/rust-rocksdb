.PHONY: prebuild install clean

prebuild:
	@echo "Building RocksDB shared library, static library, and ldb binary..."
	@if [ ! -f "librocksdb-sys/rocksdb/Makefile" ]; then \
		echo "RocksDB submodule not found. Initializing..."; \
		git submodule update --init --recursive; \
	fi
	cd librocksdb-sys/rocksdb && env ROCKSDB_NO_FBCODE=1 DISABLE_JEMALLOC=1 EXTRA_CXXFLAGS="$${EXTRA_CXXFLAGS:-} -I/usr/local/include -Wno-error=unused-parameter" EXTRA_LDFLAGS="-L/usr/local/lib" PORTABLE=0 USE_RTTI=1 make shared_lib static_lib -j$$(nproc)
	cd librocksdb-sys/rocksdb && env DISABLE_WARNING_AS_ERROR=1 DEBUG_LEVEL=0 USE_RTTI=1 make ldb
	@echo ""
	@echo "Prebuild complete! Run 'make install' to install natively."

install:
	@echo "Installing RocksDB to /usr/local/zaidoon... (Requires sudo)"
	cd librocksdb-sys/rocksdb && sudo make install-shared INSTALL_PATH=/usr/local/zaidoon
	cd librocksdb-sys/rocksdb && sudo make install-static INSTALL_PATH=/usr/local/zaidoon
	sudo mkdir -p /usr/local/zaidoon/bin
	sudo cp -p librocksdb-sys/rocksdb/ldb /usr/local/zaidoon/bin/ldb
	sudo ldconfig /usr/local/zaidoon/lib || true
	@echo "============================================================"
	@echo "Installation complete!"
	@echo "To use these cached libraries and bypass Cargo recompiling"
	@echo "the C++ dependencies, export the following in your shell:"
	@echo ""
	@echo "    export PATH=\$$PATH:/usr/local/zaidoon/bin"
	@echo ""
	@echo "A Cargo override config needs to be created in .cargo/config.toml."
	@echo "Please see CONTRIBUTING.md for the suggested configuration to link"
	@echo "Cargo to natively built libraries."
	@echo "============================================================"

clean:
	cd librocksdb-sys/rocksdb && make clean

Creates a new column family by importing a collection of SST files

Column family SST files can be created in one of the following ways:

1. Directly using [`SstFileWriter`](crate::SstFileWriter).
2. Exported from an existing DB using [`Checkpoint::export_column_family`][export_cf]. In that case,
   `metadata` should be the output of [`export_column_family`][export_cf].

The parameter `import_options` can specify whether to copy or move the external files (default is to
copy). If set to copy, managing the source files is the caller's responsibility. When set to move,
the operation will attempt to delete the external files on successful import, logging any failure to
delete rather than returning an error. Files are not modified on any error, and a best effort is
made to remove any newly-created files. A new Column Family will be present on a successful return,
and will not be present on error. A Column Family may be present if a crash occurs during a call.

# Examples

```rust
use rust_rocksdb::{ExportImportFilesMetaData, ImportColumnFamilyOptions, Options, DB};

fn import_column_family(
    db: &mut DB,
    column_family_name: &str,
    import_metadata: &ExportImportFilesMetaData,
) {
    let cf_opts = Options::default();

    let mut import_opts = ImportColumnFamilyOptions::default();
    import_opts.set_move_files(true);

    db.create_column_family_with_import(
        &cf_opts,
        column_family_name,
        &import_opts,
        &import_metadata,
    )
        .unwrap();
}
```

[export_cf]: crate::checkpoint::Checkpoint::export_column_family

// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Consolidated compile-fail tests using trybuild.
//!
//! All compile-fail tests are consolidated into a single test to avoid
//! multiple expensive recompilations. Each trybuild::TestCases::new()
//! triggers compilation of the crate, and when tests run in parallel,
//! this can cause severe slowdowns (360+ seconds per test).
//!
//! By consolidating all compile-fail tests into one test function,
//! we share the compilation cache and run all tests efficiently.

/// Single consolidated test for all compile-fail scenarios.
///
/// This test verifies that various lifetime violations are caught at compile time:
/// - Checkpoint cannot outlive its DB
/// - Iterator cannot outlive its DB
/// - Snapshot cannot outlive its DB
/// - SingleThreaded DB cannot have multiple mutable references
/// - Transaction cannot outlive TransactionDB
/// - Snapshot cannot outlive TransactionDB
/// - Snapshot cannot outlive Transaction
#[test]
fn test_compile_fail() {
    let t = trybuild::TestCases::new();

    // DB lifetime tests
    t.compile_fail("tests/fail/checkpoint_outlive_db.rs");
    t.compile_fail("tests/fail/iterator_outlive_db.rs");
    t.compile_fail("tests/fail/snapshot_outlive_db.rs");
    t.compile_fail("tests/fail/open_with_multiple_refs_as_single_threaded.rs");

    // TransactionDB lifetime tests
    t.compile_fail("tests/fail/snapshot_outlive_transaction_db.rs");
    t.compile_fail("tests/fail/transaction_outlive_transaction_db.rs");
    t.compile_fail("tests/fail/snapshot_outlive_transaction.rs");
}

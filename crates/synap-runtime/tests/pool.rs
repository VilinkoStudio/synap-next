use std::sync::Arc;
use std::thread::{self, ThreadId};

use synap_runtime::{Definitions, Pool, Registry};

#[test]
fn workers_run_jobs_on_their_own_threads() {
    let definitions = Arc::new(
        Definitions::load([b"plugin \"test.pool\" { api = 1 }".as_slice()]).expect("definitions"),
    );
    let pool = Pool::new(definitions, 2).expect("pool");
    let job: fn(&Registry) -> ThreadId = |_| thread::current().id();
    let threads = pool.run_many(vec![job, job]);
    let first = threads[0].as_ref().expect("first").to_owned();
    let second = threads[1].as_ref().expect("second").to_owned();
    assert_ne!(first, second);
    assert_ne!(first, thread::current().id());
}

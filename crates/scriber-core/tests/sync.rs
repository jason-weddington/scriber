//! The value that loads once: the speech model, shared by the warm-up and
//! every transcription.

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    use scriber_core::sync::{LoadOnce, lock};

    type Got = Result<u32, String>;

    #[test]
    fn it_loads_on_first_use_and_keeps_the_value() {
        let once = LoadOnce::<u32>::default();
        let loads = AtomicU32::new(0);
        let mut load = || -> Got {
            let _n = loads.fetch_add(1, Ordering::SeqCst);
            Ok(7)
        };
        let mut read = |v: &mut u32| -> Got { Ok(*v) };
        assert_eq!(once.with(&mut load, &mut read), Ok(7), "loaded");
        let mut bump = |v: &mut u32| -> Got {
            *v += 1;
            Ok(*v)
        };
        assert_eq!(
            once.with(&mut load, &mut bump),
            Ok(8),
            "kept, and work can change it"
        );
        assert_eq!(once.with(&mut load, &mut read), Ok(8), "the change is kept");
        assert_eq!(loads.load(Ordering::SeqCst), 1, "loaded once");
    }

    #[test]
    fn a_failed_load_is_tried_again_and_a_failed_work_keeps_the_value() {
        let once = LoadOnce::<u32>::default();
        let mut read = |v: &mut u32| -> Got { Ok(*v) };
        let mut corrupt = || -> Got { Err("corrupt".to_string()) };
        let mut three = || -> Got { Ok(3) };
        let mut nine = || -> Got { Ok(9) };
        let mut refuse = |_: &mut u32| -> Got { Err("bad input".to_string()) };
        assert_eq!(
            once.with(&mut corrupt, &mut read),
            Err("corrupt".to_string()),
            "the load's error"
        );
        assert_eq!(once.with(&mut three, &mut read), Ok(3), "tried again");
        assert_eq!(
            once.with(&mut nine, &mut refuse),
            Err("bad input".to_string()),
            "the work's error"
        );
        assert_eq!(once.with(&mut nine, &mut read), Ok(3), "still loaded");
    }

    #[test]
    fn a_caller_that_arrives_during_a_load_waits_for_it_and_does_not_load_again() {
        let once = Arc::new(LoadOnce::<u32>::default());
        let loads = Arc::new(AtomicU32::new(0));
        let (started, wait_started) = mpsc::channel();
        let (go, wait_go) = mpsc::channel::<()>();
        let warm = {
            let (once, loads) = (Arc::clone(&once), Arc::clone(&loads));
            thread::spawn(move || -> Got {
                let mut load = || -> Got {
                    let _n = loads.fetch_add(1, Ordering::SeqCst);
                    started.send(()).unwrap();
                    wait_go.recv().unwrap();
                    Ok(5)
                };
                once.with(&mut load, &mut |v: &mut u32| -> Got { Ok(*v) })
            })
        };
        wait_started.recv().unwrap();
        let (finished, wait_finished) = mpsc::channel();
        let transcribe = {
            let (once, loads) = (Arc::clone(&once), Arc::clone(&loads));
            thread::spawn(move || {
                let mut load = || -> Got {
                    let _n = loads.fetch_add(1, Ordering::SeqCst);
                    Ok(6)
                };
                let got = once.with(&mut load, &mut |v: &mut u32| -> Got { Ok(*v) });
                finished.send(got).unwrap();
            })
        };
        assert!(
            wait_finished
                .recv_timeout(Duration::from_millis(100))
                .is_err(),
            "the second caller waits while the first loads"
        );
        go.send(()).unwrap();
        assert_eq!(warm.join().unwrap(), Ok(5), "the warm-up loaded");
        assert_eq!(
            wait_finished.recv().unwrap(),
            Ok(5),
            "the first load's value"
        );
        transcribe.join().unwrap();
        assert_eq!(loads.load(Ordering::SeqCst), 1, "loaded once");
    }

    #[test]
    fn lock_carries_on_past_a_panic() {
        let shared = Arc::new(Mutex::new(1));
        let clone = Arc::clone(&shared);
        let panicked = thread::spawn(move || {
            let _held = clone.lock().unwrap();
            panic!("poisoned on purpose");
        })
        .join();
        assert!(panicked.is_err(), "the thread panicked holding the lock");
        assert_eq!(*lock(&shared), 1, "the value is still there");
    }
}

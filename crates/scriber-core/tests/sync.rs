//! The value that loads once: the speech model, shared by the warm-up and
//! every transcription.

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    use scriber_core::ports::PortError;
    use scriber_core::sync::{Closed, LoadOnce, lock};

    type Got = Result<u32, PortError>;

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
        let mut corrupt = || -> Got { Err(PortError("corrupt".to_string())) };
        let mut three = || -> Got { Ok(3) };
        let mut nine = || -> Got { Ok(9) };
        let mut refuse = |_: &mut u32| -> Got { Err(PortError("bad input".to_string())) };
        assert_eq!(
            once.with(&mut corrupt, &mut read),
            Err(PortError("corrupt".to_string())),
            "the load's error"
        );
        assert_eq!(once.with(&mut three, &mut read), Ok(3), "tried again");
        assert_eq!(
            once.with(&mut nine, &mut refuse),
            Err(PortError("bad input".to_string())),
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

    fn shut() -> PortError {
        PortError("the engine has shut down".to_string())
    }

    #[test]
    fn closed_is_a_port_error_naming_the_shutdown() {
        assert_eq!(PortError::from(Closed), shut(), "the sentence");
    }

    #[test]
    fn close_frees_the_value_once() {
        let empty = LoadOnce::<Arc<()>>::default();
        assert!(!empty.close(), "nothing loaded, nothing freed");
        let once = LoadOnce::<Arc<()>>::default();
        let value = Arc::new(());
        let mut load = || -> Result<Arc<()>, PortError> { Ok(Arc::clone(&value)) };
        once.with(&mut load, &mut |_: &mut Arc<()>| -> Result<(), PortError> {
            Ok(())
        })
        .unwrap();
        assert_eq!(Arc::strong_count(&value), 2, "loaded and kept");
        assert!(once.close(), "the loaded value is freed");
        assert_eq!(Arc::strong_count(&value), 1, "dropped before close returns");
        assert!(!once.close(), "already closed");
    }

    #[test]
    fn nothing_loads_after_close() {
        let once = LoadOnce::<u32>::default();
        let (loads, works) = (AtomicU32::new(0), AtomicU32::new(0));
        assert!(!once.close(), "nothing loaded");
        let got = once.with(
            &mut || -> Got {
                let _n = loads.fetch_add(1, Ordering::SeqCst);
                Ok(1)
            },
            &mut |v: &mut u32| -> Got {
                let _n = works.fetch_add(1, Ordering::SeqCst);
                Ok(*v)
            },
        );
        assert_eq!(got, Err(shut()), "refused");
        assert_eq!(loads.load(Ordering::SeqCst), 0, "no load");
        assert_eq!(works.load(Ordering::SeqCst), 0, "no work");
    }

    /// A thread inside `work` on `once`, until a message arrives on the
    /// returned sender; returns once it is inside.
    fn held(once: &Arc<LoadOnce<u32>>) -> (mpsc::Sender<()>, thread::JoinHandle<Got>) {
        let (inside, wait_inside) = mpsc::channel();
        let (release, wait_release) = mpsc::channel::<()>();
        let once = Arc::clone(once);
        let a = thread::spawn(move || {
            once.with(&mut || -> Got { Ok(4) }, &mut |v: &mut u32| -> Got {
                inside.send(()).unwrap();
                wait_release.recv().unwrap();
                Ok(*v)
            })
        });
        wait_inside.recv().unwrap();
        (release, a)
    }

    fn closing(once: &Arc<LoadOnce<u32>>) -> mpsc::Receiver<bool> {
        let (closed, wait_closed) = mpsc::channel();
        let once = Arc::clone(once);
        let _c = thread::spawn(move || closed.send(once.close()).unwrap());
        wait_closed
    }

    #[test]
    fn close_waits_for_work_in_progress() {
        let once = Arc::new(LoadOnce::<u32>::default());
        let (release, a) = held(&once);
        let wait_closed = closing(&once);
        assert!(
            wait_closed
                .recv_timeout(Duration::from_millis(100))
                .is_err(),
            "close waits for the work"
        );
        release.send(()).unwrap();
        assert_eq!(a.join().unwrap(), Ok(4), "the work finished");
        assert_eq!(
            wait_closed.recv_timeout(Duration::from_secs(5)),
            Ok(true),
            "then the value was freed"
        );
    }

    #[test]
    fn callers_waiting_for_the_value_are_refused_once_closing() {
        let once = Arc::new(LoadOnce::<u32>::default());
        let (release, a) = held(&once);
        let counts = Arc::new((AtomicU32::new(0), AtomicU32::new(0)));
        let b = {
            let (once, counts) = (Arc::clone(&once), Arc::clone(&counts));
            thread::spawn(move || {
                once.with(
                    &mut || -> Got {
                        let _n = counts.0.fetch_add(1, Ordering::SeqCst);
                        Ok(5)
                    },
                    &mut |v: &mut u32| -> Got {
                        let _n = counts.1.fetch_add(1, Ordering::SeqCst);
                        Ok(*v)
                    },
                )
            })
        };
        let wait_closed = closing(&once);
        thread::sleep(Duration::from_millis(100));
        release.send(()).unwrap();
        assert_eq!(a.join().unwrap(), Ok(4), "the work finished");
        assert_eq!(b.join().unwrap(), Err(shut()), "the waiter is refused");
        assert_eq!(counts.0.load(Ordering::SeqCst), 0, "no load");
        assert_eq!(counts.1.load(Ordering::SeqCst), 0, "no work");
        assert_eq!(
            wait_closed.recv_timeout(Duration::from_secs(5)),
            Ok(true),
            "the value was freed"
        );
    }
}

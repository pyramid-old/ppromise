#![feature(core, core_slice_ext, box_patterns, cell_extras, unboxed_closures, fnbox)]
extern crate core;
extern crate threadpool;

use std::mem;
use std::rc::Rc;
use std::cell::RefCell;
use std::cell::Ref;
use std::boxed::FnBox;
use core::slice::SliceExt;
use std::thread;
use std::sync::mpsc;
use std::sync::mpsc::*;
use threadpool::ThreadPool;

pub struct Promise<T> {
    state: Rc<RefCell<PromiseState<T>>>
}

impl<T: 'static> Promise<T> {
    pub fn new() -> Promise<T> {
        Promise {
            state: Rc::new(RefCell::new(PromiseState::Unresolved))
        }
    }
    pub fn resolved(value: T) -> Promise<T> {
        Promise {
            state: Rc::new(RefCell::new(PromiseState::Resolved(value)))
        }
    }
    pub fn resolve(&mut self, value: T) {
        self.state.resolve(value);
    }
    pub fn value(&self) -> Option<Ref<T>> {
        Ref::filter_map(self.state.borrow(), |state| match state {
            &PromiseState::Resolved(ref value) => Some(value),
            _ => None
        })
    }
    pub fn into_value(self) -> T {
        let mut s = self.state.borrow_mut();
        let state = mem::replace(&mut *s, PromiseState::Moved);
        match state {
            PromiseState::Resolved(value) => value,
            _ => panic!("Trying to call into_value on non-value promise.")
        }
    }
    pub fn then_move<T2: 'static, F: FnOnce(T) -> T2 + 'static>(&mut self, transform: F) -> Promise<T2> {
        let p = Promise::<T2>::new();
        let p_state = p.state.clone();
        self._then_move(move |value| {
            p_state.resolve(transform(value));
        });
        p
    }
    pub fn then<T2: 'static, F: FnOnce(&T) -> T2 + 'static>(&mut self, transform: F) -> Promise<T2> {
        let p = Promise::<T2>::new();
        let p_state = p.state.clone();
        self._then(move |value| {
            p_state.resolve(transform(value));
        });
        p
    }
    pub fn then_move_promise<T2: 'static, F: FnOnce(T) -> Promise<T2> + 'static>(&mut self, transform: F) -> Promise<T2> {
        let p = Promise::<T2>::new();
        let p_state = p.state.clone();
        self._then_move(move |value| {
            let mut p2 = transform(value);
            p2._then_move(move |v2| {
                p_state.resolve(v2);
            });
        });
        p
    }
    pub fn then_promise<T2: 'static, F: FnOnce(&T) -> Promise<T2> + 'static>(&mut self, transform: F) -> Promise<T2> {
        let p = Promise::<T2>::new();
        let p_state = p.state.clone();
        self._then(move |value| {
            let mut p2 = transform(value);
            p2._then_move(move |v2| {
                p_state.resolve(v2);
            });
        });
        p
    }
    fn _then_move<F: FnOnce(T) -> () + 'static>(&mut self, transform: F) {
        if self.state.borrow().is_moved() {
            panic!("Trying to move promise value that has already been moved.");
        }
        if self.state.borrow().is_resolved() {
            let mut s = self.state.borrow_mut();
            if let PromiseState::Resolved(value) = mem::replace(&mut *s, PromiseState::Moved) {
                return transform(value);
            } else {
                unreachable!();
            }
        }
        let mut s = self.state.borrow_mut();
        let state = mem::replace(&mut *s, PromiseState::Unresolved);
        *s = state.insert_then_move(move |value: T| {
            transform(value);
        });
    }
    fn _then<F: FnOnce(&T) -> () + 'static>(&mut self, transform: F) {
        if self.state.borrow().is_moved() {
            panic!("Trying to borrow promise value that has already been moved.");
        }
        if let &PromiseState::Resolved(ref value) = &*self.state.borrow() {
            return transform(value);
        }
        let mut s = self.state.borrow_mut();
        let state = mem::replace(&mut *s, PromiseState::Unresolved);
        *s = state.insert_then(move |value: &T| {
            transform(value);
        });
    }
}

pub fn join<T1: 'static, T2: 'static>(p1: &mut Promise<T1>, p2: &mut Promise<T2>) -> Promise<(T1, T2)> {
    (p1, p2).join()
}
pub fn join3<T1: 'static, T2: 'static, T3: 'static>(p1: &mut Promise<T1>, p2: &mut Promise<T2>, p3: &mut Promise<T3>) -> Promise<(T1, T2, T3)> {
    (p1, p2, p3).join()
}

pub trait Joinable<T> {
    fn join(self) -> Promise<T>;
}

impl<'a, T: 'static> Joinable<Vec<T>> for Vec<Promise<T>> {
    fn join(mut self) -> Promise<Vec<T>> {
        self.iter_mut().collect::<Vec<&mut Promise<T>>>().join()
    }
}

impl<'a, T: 'static> Joinable<Vec<T>> for Vec<&'a mut Promise<T>> {
    fn join(mut self) -> Promise<Vec<T>> {
        let mut p: Promise<Vec<T>> = self[0].then_move(|x| vec![x]);
        for i in 1..self.len() {
            let mut p2 = &mut self[i];
            p = p2.then_move_promise(move |x| {
                p.then_move(move |mut xs: Vec<T>| { xs.push(x); xs })
            });
        }
        p
    }
}

impl<'a, T1: 'static, T2: 'static> Joinable<(T1, T2)> for (&'a mut Promise<T1>, &'a mut Promise<T2>) {
    fn join(mut self) -> Promise<(T1, T2)> {
        let mut p1 = Promise { state: self.1.state.clone() };
        self.0.then_move_promise(move |x1| {
            p1.then_move(move |x2| {
                (x1, x2)
            })
        })
    }
}

impl<'a, T1: 'static, T2: 'static, T3: 'static> Joinable<(T1, T2, T3)> for (&'a mut Promise<T1>, &'a mut Promise<T2>, &'a mut Promise<T3>) {
    fn join(mut self) -> Promise<(T1, T2, T3)> {
        let mut p1 = Promise { state: self.1.state.clone() };
        let mut p2 = Promise { state: self.2.state.clone() };
        self.0.then_move_promise(move |x1| {
            p1.then_move_promise(move |x2| {
                p2.then_move(move |x3| {
                    (x1, x2, x3)
                })
            })
        })
    }
}


trait Resolveable {
    fn try_resolve(&self) -> bool;
}

struct Running<T> {
    receiver: Receiver<T>,
    promise_state: Rc<RefCell<PromiseState<T>>>
}

impl<T: 'static> Resolveable for Running<T> {
    fn try_resolve(&self) -> bool {
        match self.receiver.try_recv() {
            Ok(value) => {
                self.promise_state.resolve(value);
                true
            },
            _ => false
        }
    }
}

pub struct AsyncRunner {
    running: Vec<Box<Resolveable>>,
    pool: Option<ThreadPool>
}
impl AsyncRunner {
    pub fn new() -> AsyncRunner {
        AsyncRunner {
            running: vec![],
            pool: None
        }
    }
    pub fn new_pooled(threads: usize) -> AsyncRunner {
        AsyncRunner {
            running: vec![],
            pool: Some(ThreadPool::new(threads))
        }
    }
    pub fn exec_async<T: Send + Sized + 'static, F: Fn() -> T + Send + Sized + 'static>(&mut self, run: F) -> Promise<T> {
        let (tx, rx) = mpsc::channel();

        let f = move || {
            match tx.send(run()) {
                Ok(()) => {},
                Err(err) => panic!("Thread error: {}", err)
            }
        };

        if let &Some(ref pool) = &self.pool {
            pool.execute(f);
        } else {
            thread::spawn(f);
        }

        let promise = Promise::new();
        self.running.push(Box::new(Running { receiver: rx, promise_state: promise.state.clone() }));
        promise
    }
    pub fn try_resolve_all(&mut self) {
        let running = mem::replace(&mut self.running, Vec::new());
        self.running = running.into_iter().filter(|r| !r.try_resolve()).collect();
    }
}


enum PromiseState<T> {
    Unresolved,
    Moved,
    Resolved(T),
    Then(Vec<Box<FnBox(&T) -> ()>>, Box<PromiseState<T>>),
    ThenMove(Box<FnBox(T) -> ()>)
}

impl<T> PromiseState<T> {
    fn is_resolved(&self) -> bool {
        if let &PromiseState::Resolved(_) = self {
            true
        } else {
            false
        }
    }
    fn is_moved(&self) -> bool {
        if let &PromiseState::Moved = self {
            true
        } else {
            false
        }
    }
    fn insert_then<F: FnOnce(&T) -> () + 'static>(self, transform: F) -> PromiseState<T> {
        match self {
            PromiseState::Unresolved => PromiseState::Then(vec![Box::new(transform)], Box::new(PromiseState::Unresolved)),
            PromiseState::Then(mut ts, then) => {
                ts.push(Box::new(transform));
                PromiseState::Then(ts, then)
            },
            PromiseState::ThenMove(t) => {
                PromiseState::Then(vec![Box::new(transform)], Box::new(PromiseState::ThenMove(t)))
            },
            _ => unreachable!()
        }
    }
    fn insert_then_move<F: FnOnce(T) -> () + 'static>(self, transform: F) -> PromiseState<T> {
        match self {
            PromiseState::Unresolved => PromiseState::ThenMove(Box::new(transform)),
            PromiseState::Then(ts, box then) => {
                PromiseState::Then(ts, Box::new(then.insert_then_move(transform)))
            },
            PromiseState::ThenMove(_) => {
                panic!("Cannot move value out of promise twice.");
            },
            _ => unreachable!()
        }
    }
    fn transform(self, value: T) -> PromiseState<T> {
        match self {
            PromiseState::Unresolved => PromiseState::Resolved(value),
            PromiseState::Then(transforms, box then) => {
                for transform in transforms {
                    transform.call_box((&value,));
                }
                then.transform(value)
            },
            PromiseState::ThenMove(transform) => {
                transform(value);
                PromiseState::Unresolved
            },
            _ => unreachable!()
        }
    }
}

trait ResolvableState<T> {
    fn resolve(&self, value: T);
}
impl<T> ResolvableState<T> for Rc<RefCell<PromiseState<T>>> {
    fn resolve(&self, value: T) {
        let mut s = self.borrow_mut();
        let state = mem::replace(&mut *s, PromiseState::Unresolved);
        *s = state.transform(value);
    }
}

#[test]
fn test_promise_resolve() {
    let mut p = Promise::new();
    p.resolve(5);
    assert_eq!(*p.value().unwrap(), 5);
}

#[test]
fn test_promise_resolved_then_promise() {
    let mut p = Promise::resolved(5);
    let p2 = p.then_promise(|val| Promise::resolved(val * 2));
    assert_eq!(*p2.value().unwrap(), 10);
}

#[test]
fn test_promise_then_promise() {
    let mut p = Promise::new();
    let p2 = p.then_promise(|val| Promise::resolved(val * 2));
    p.resolve(5);
    assert_eq!(*p2.value().unwrap(), 10);
}

#[test]
fn test_promise_resolved_then_move_promise() {
    let mut p = Promise::resolved(5);
    let p2 = p.then_move_promise(|val| Promise::resolved(val * 2));
    assert_eq!(*p2.value().unwrap(), 10);
}

#[test]
fn test_promise_then_move_promise() {
    let mut p = Promise::new();
    let p2 = p.then_move_promise(|val| Promise::resolved(val * 2));
    p.resolve(5);
    assert_eq!(*p2.value().unwrap(), 10);
}

#[test]
fn test_promise_then() {
    let mut p = Promise::new();
    let p2 = p.then(|val| val * 2);
    p.resolve(5);
    assert_eq!(*p2.value().unwrap(), 10);
}

#[test]
fn test_then_after_resolved() {
    let mut p = Promise::new();
    p.resolve(7);
    let p2 = p.then(|x| x * 2);
    assert_eq!(*p2.value().unwrap(), 14);
}

#[test]
fn test_promise_then_then_move() {
    let mut p = Promise::new();
    let p2 = p.then(|val| val * 2);
    let p3 = p.then_move(|val| val * 3);
    p.resolve(5);
    assert!(p.value().is_none());
    assert_eq!(*p2.value().unwrap(), 10);
    assert_eq!(*p3.value().unwrap(), 15);
}

#[test]
#[should_panic]
fn test_promise_then_move_then_move() {
    let mut p = Promise::<i32>::new();
    p.then_move(|val| val * 2);
    p.then_move(|val| val * 3);
}

#[test]
#[should_panic]
fn test_promise_resolved_then_move_then_move() {
    let mut p = Promise::resolved(5);
    p.then_move(|val| val * 2);
    p.then_move(|val| val * 3);
}

#[test]
#[should_panic]
fn test_promise_resolved_then_move_then() {
    let mut p = Promise::resolved(5);
    p.then_move(|val| val * 2);
    p.then(|val| val * 3);
}

#[test]
fn test_promise_join() {
    let mut a: Promise<i32> = Promise::new();
    let mut b: Promise<String> = Promise::new();
    let j = (&mut a, &mut b).join().then(|&(ref i, ref s)| format!("{} _ {}", i, s));
    assert!(j.value().is_none());
    a.resolve(5);
    assert!(j.value().is_none());
    b.resolve("hello".to_string());
    assert_eq!(*j.value().unwrap(), "5 _ hello".to_string());
}

#[test]
fn test_promise_join3() {
    let mut a: Promise<i32> = Promise::new();
    let mut b: Promise<String> = Promise::new();
    let mut c: Promise<String> = Promise::new();
    let j = (&mut a, &mut b, &mut c).join().then(|&(ref i, ref s, ref s2)| format!("{} _ {} {}", i, s, s2));
    assert!(j.value().is_none());
    a.resolve(5);
    assert!(j.value().is_none());
    b.resolve("hello".to_string());
    c.resolve("world".to_string());
    assert_eq!(*j.value().unwrap(), "5 _ hello world".to_string());
}

#[test]
fn test_promise_array_join() {
    let mut a: Promise<i32> = Promise::new();
    let mut b: Promise<i32> = Promise::new();
    let j: Promise<Vec<i32>> = vec![&mut a, &mut b].join();
    assert!(j.value().is_none());
    a.resolve(5);
    assert!(j.value().is_none());
    b.resolve(7);
    assert_eq!(*j.value().unwrap(), vec![5, 7]);
}

#[test]
fn test_promise_async() {
    let mut runner = AsyncRunner::new();
    let p = runner.exec_async(|| {
        thread::sleep_ms(10);
        "Hello world from thread".to_string()
    });
    runner.try_resolve_all();
    assert!(p.value().is_none());
    thread::sleep_ms(20);
    runner.try_resolve_all();
    assert_eq!(*p.value().unwrap(), "Hello world from thread");
}

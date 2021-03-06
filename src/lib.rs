//! This crate provides approximate quantiles over data streams in a moderate
//! amount of memory.
//!
//! Order statistics is a rough business. Exact solutions are expensive in terms
//! of memory and computation. Recent literature has advanced approximations but
//! each have fundamental tradeoffs. This crate is intended to be a collection
//! of approximate algorithms that provide guarantees around space consumption.
#![deny(missing_docs)]
#![doc(html_root_url = "https://postmates.github.io/quantiles/")]

include!(concat!(env!("OUT_DIR"), "/ckms_types.rs"));

use std::fmt::Debug;
use std::cmp;
use std::ops::{AddAssign, Add};

#[cfg(test)]
#[macro_use]
extern crate quickcheck;

pub mod misra_gries;
pub mod greenwald_khanna;

impl<T> AddAssign for CKMS<T> 
    where T: Copy + Add<Output = T> + PartialOrd + Debug 
{
    fn add_assign(&mut self, rhs: CKMS<T>) {
        self.last_in = rhs.last_in;
        self.sum = match (self.sum, rhs.sum) {
            (None, None) => None,
            (None, Some(y)) => Some(y),
            (Some(x), None) => Some(x),
            (Some(x), Some(y)) => Some(x.add(y)),
        };
        for smpl in rhs.samples {
            self.priv_insert(smpl.v);
        }
    }
}

impl<T: Copy + PartialOrd + Debug + Add<Output = T>> CKMS<T> {
    /// Create a new CKMS
    ///
    /// A CKMS is meant to answer quantile queries with a known error bound. If
    /// the error passed here is ε and there have been `n` items inserted into
    /// CKMS then for any quantile query Φ the deviance from the true quantile
    /// will be +/- εΦn.
    ///
    /// For an error ε this structure will require T*(floor(1/(2*ε)) + O(1/ε log
    /// εn)) + f64 + usize + usize words of storage.
    ///
    /// # Examples
    /// ```
    /// use quantiles::CKMS;
    ///
    /// let mut ckms = CKMS::<u64>::new(0.001);
    /// for i in 1..1001 {
    ///     ckms.insert(i as u64);
    /// }
    /// assert_eq!(ckms.query(0.0), Some((1, 1)));
    /// assert_eq!(ckms.query(0.998), Some((998, 998)));
    /// assert_eq!(ckms.query(0.999), Some((999, 999)));
    /// assert_eq!(ckms.query(1.0), Some((1000, 1000)));
    /// ```
    ///
    /// `error` must but a value between 0 and 1, exclusive of both extremes. If
    /// you input an error <= 0.0 CKMS will assign an error of
    /// 0.00000001. Likewise, if your error is >= 1.0 CKMS will assign an error
    /// of 0.99.
    pub fn new(error: f64) -> CKMS<T> {
        let error = if error <= 0.0 {
            0.00000001
        } else if error >= 1.0 {
            0.99
        } else {
            error
        };
        let insert_threshold = 1.0 / (2.0 * error);
        let insert_threshold = if insert_threshold < 1.0 {
            1.0
        } else {
            insert_threshold
        };
        CKMS {
            n: 0,

            error: error,

            insert_threshold: insert_threshold as usize,
            inserts: 0,

            samples: Vec::<Entry<T>>::new(),

            last_in: None,
            sum: None,
        }
    }

    /// Return the last element added to the CKMS
    ///
    /// # Example
    /// ```
    /// use quantiles::CKMS;
    ///
    /// let mut ckms = CKMS::new(0.1);
    /// ckms.insert(1.0);
    /// ckms.insert(2.0);
    /// ckms.insert(3.0);
    /// assert_eq!(Some(3.0), ckms.last());
    /// ```
    pub fn last(&self) -> Option<T> {
        self.last_in
    }

    /// Return the sum of the elements added to the CKMS
    ///
    /// # Example
    /// ```
    /// use quantiles::CKMS;
    ///
    /// let mut ckms = CKMS::new(0.1);
    /// ckms.insert(1.0);
    /// ckms.insert(2.0);
    /// ckms.insert(3.0);
    /// assert_eq!(Some(6.0), ckms.sum());
    /// ```
    pub fn sum(&self) -> Option<T> {
        self.sum
    }

    /// Insert a T into the CKMS
    ///
    /// Insertion will gradulally shift the approximate quantiles. This
    /// implementation is biased toward fast writes and slower queries. Storage
    /// may grow gradually, as defined in the module-level documentation, but
    /// will remain bounded.
    pub fn insert(&mut self, v: T) {
        self.sum = self.sum.map_or(Some(v), |s| Some(s.add(v)));
        self.last_in = Some(v);
        self.priv_insert(v);
    }

    fn priv_insert(&mut self, v: T) {
        let s = self.samples.len();
        let mut r = 0;
        if s == 0 {
            self.samples.insert(0,
                                Entry {
                                    v: v,
                                    g: 1,
                                    delta: 0,
                                });
            self.n += 1;
            return;
        }

        let mut idx = 0;
        for i in 0..s {
            let smpl = &self.samples[i];
            match smpl.v.partial_cmp(&v) {
                Some(cmp::Ordering::Less) => idx += 1,
                _ => break,
            }
            r += smpl.g;
        }
        let delta = if idx == 0 || idx == s {
            0
        } else {
            self.invariant(r as f64) - 1
        };
        self.samples.insert(idx,
                            Entry {
                                v: v,
                                g: 1,
                                delta: delta,
                            });
        self.n += 1;
        self.inserts = (self.inserts + 1) % self.insert_threshold;
        if self.inserts == 0 {
            self.compress();
        }
    }

    /// Query CKMS for a ε-approximate quantile
    ///
    /// This function returns an approximation to the true quantile-- +/- εΦn
    /// --for the points inserted. Argument q is valid 0. <= q <= 1.0. The
    /// minimum and maximum quantile, corresponding to 0.0 and 1.0 respectively,
    /// are always known precisely.
    ///
    /// Return
    ///
    /// # Examples
    /// ```
    /// use quantiles::CKMS;
    ///
    /// let mut ckms = CKMS::<u32>::new(0.001);
    /// for i in 0..1000 {
    ///     ckms.insert(i as u32);
    /// }
    ///
    /// assert_eq!(ckms.query(0.0), Some((1, 0)));
    /// assert_eq!(ckms.query(0.998), Some((998, 997)));
    /// assert_eq!(ckms.query(1.0), Some((1000, 999)));
    /// ```
    pub fn query(&self, q: f64) -> Option<(usize, T)> {
        let s = self.samples.len();

        if s == 0 {
            return None;
        }

        let mut r = 0;
        let nphi = q * (self.n as f64);
        for i in 1..s {
            let prev = &self.samples[i - 1];
            let cur = &self.samples[i];

            r += prev.g;

            let lhs = (r + cur.g + cur.delta) as f64;
            let rhs = nphi + ((self.invariant(nphi) as f64) / 2.0);

            if lhs > rhs {
                return Some((r, prev.v));
            }
        }

        let v = self.samples[s - 1].v;
        Some((s, v))
    }

    /// Query CKMS for the count of its points
    ///
    /// This function returns the total number of points seen over the lifetime
    /// of the datastructure, _not_ the number of points currently stored in the
    /// structure.
    ///
    /// # Examples
    /// ```
    /// use quantiles::CKMS;
    ///
    /// let mut ckms = CKMS::<u32>::new(0.001);
    /// for i in 0..1000 {
    ///     ckms.insert(i as u32);
    /// }
    ///
    /// assert_eq!(ckms.count(), 1000);
    /// ```
    pub fn count(&self) -> usize {
        self.n
    }

    #[inline]
    fn invariant(&self, r: f64) -> usize {
        let i = (2.0 * self.error * r).floor() as usize;
        if 1 > i { 1 } else { i }
    }

    fn compress(&mut self) {
        if self.samples.len() < 3 {
            return;
        }

        let mut s_mx = self.samples.len() - 1;
        let mut i = 0;
        let mut r = 1;

        loop {
            let cur_g = self.samples[i].g;
            let nxt_v = self.samples[i + 1].v;
            let nxt_g = self.samples[i + 1].g;
            let nxt_delta = self.samples[i + 1].delta;

            if cur_g + nxt_g + nxt_delta <= self.invariant(r as f64) {
                let ent = Entry {
                    v: nxt_v,
                    g: nxt_g + cur_g,
                    delta: nxt_delta,
                };
                self.samples[i] = ent;
                self.samples.remove(i + 1);
                s_mx -= 1;
            } else {
                i += 1;
            }
            r += 1;

            if i == s_mx {
                break;
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use super::quickcheck::{QuickCheck, TestResult};
    use std::f64::consts::E;

    fn percentile(data: &Vec<f64>, prcnt: f64) -> f64 {
        let idx = (prcnt * (data.len() as f64)) as usize;
        return data[idx];
    }

    #[test]
    fn error_nominal_test() {
        fn inner(mut data: Vec<f64>, prcnt: f64) -> TestResult {
            data.sort_by(|a, b| a.partial_cmp(b).unwrap());
            if !(prcnt >= 0.0) || !(prcnt <= 1.0) {
                return TestResult::discard();
            } else if data.len() < 1 {
                return TestResult::discard();
            }
            let err = 0.001;

            let mut ckms = CKMS::<f64>::new(err);
            for d in &data {
                ckms.insert(*d);
            }

            if let Some((_, v)) = ckms.query(prcnt) {
                debug_assert!((v - percentile(&data, prcnt)) < err,
                              "v: {} | percentile: {} | prcnt: {} | data: {:?}",
                              v,
                              percentile(&data, prcnt),
                              prcnt,
                              data);
                TestResult::passed()
            } else {
                TestResult::failed()
            }
        }
        QuickCheck::new()
            .tests(10000)
            .max_tests(100000)
            .quickcheck(inner as fn(Vec<f64>, f64) -> TestResult);
    }

    #[test]
    fn error_nominal_with_merge_test() {
        fn inner(lhs: Vec<f64>, rhs: Vec<f64>, prcnt: f64, err: f64) -> TestResult {
            if !(prcnt >= 0.0) || !(prcnt <= 1.0) {
                return TestResult::discard();
            } else if !(err >= 0.0) || !(err <= 1.0) {
                return TestResult::discard();
            } else if (lhs.len() + rhs.len()) < 1 {
                return TestResult::discard();
            }
            let mut data = lhs.clone();
            data.append(&mut rhs.clone());
            data.sort_by(|a, b| a.partial_cmp(b).unwrap());

            let err = 0.001;

            let mut ckms = CKMS::<f64>::new(err);
            for d in &lhs {
                ckms.insert(*d);
            }
            let mut ckms_rhs = CKMS::<f64>::new(err);
            for d in &rhs {
                ckms_rhs.insert(*d);
            }
            ckms += ckms_rhs;

            if let Some((_, v)) = ckms.query(prcnt) {
                debug_assert!((v - percentile(&data, prcnt)) < err,
                              "v: {} | percentile: {} | prcnt: {} | data: {:?}",
                              v,
                              percentile(&data, prcnt),
                              prcnt,
                              data);
                TestResult::passed()
            } else {
                TestResult::failed()
            }
        }
        QuickCheck::new()
            .tests(10000)
            .max_tests(100000)
            .quickcheck(inner as fn(Vec<f64>, Vec<f64>, f64, f64) -> TestResult);
    }

    #[test]
    fn n_invariant_test() {
        fn n_invariant(fs: Vec<i64>) -> bool {
            let l = fs.len();

            let mut ckms = CKMS::<i64>::new(0.001);
            for f in fs {
                ckms.insert(f);
            }

            ckms.count() == l
        }
        QuickCheck::new()
            .tests(10000)
            .max_tests(100000)
            .quickcheck(n_invariant as fn(Vec<i64>) -> bool);
    }

    #[test]
    fn add_assign_test() {
        fn inner(pair: (i64, i64)) -> bool {
            let mut lhs = CKMS::<i64>::new(0.001);
            lhs.insert(pair.0);
            let mut rhs = CKMS::<i64>::new(0.001);
            rhs.insert(pair.1);

            let expected: i64 = pair.0 + pair.1;
            lhs += rhs;

            if let Some(x) = lhs.sum() {
                if x == expected {
                    if let Some(y) = lhs.last() {
                        y == pair.1
                    } else {
                        false
                    }
                } else {
                    false
                }
            } else {
                false
            }
        }
        QuickCheck::new()
            .tests(10000)
            .max_tests(100000)
            .quickcheck(inner as fn((i64, i64)) -> bool);
    }

    // prop: forany phi. (phi*n - f(phi*n, n)/2) =< r_i =< (phi*n + f(phi*n, n)/2)
    #[test]
    fn query_invariant_test() {
        fn query_invariant(f: f64, fs: Vec<i64>) -> TestResult {
            if fs.len() < 1 {
                return TestResult::discard();
            }

            let phi = (1.0 / (1.0 + E.powf(f.abs()))) * 2.0;

            let mut ckms = CKMS::<i64>::new(0.001);
            for f in fs {
                ckms.insert(f);
            }

            match ckms.query(phi) {
                None => TestResult::passed(), // invariant to check here? n*phi + f > 1?
                Some((rank, _)) => {
                    let nphi = phi * (ckms.n as f64);
                    let fdiv2 = (ckms.invariant(nphi) as f64) / 2.0;
                    TestResult::from_bool(((nphi - fdiv2) <= (rank as f64)) ||
                                          ((rank as f64) <= (nphi + fdiv2)))
                }
            }
        }
        QuickCheck::new()
            .tests(10000)
            .max_tests(100000)
            .quickcheck(query_invariant as fn(f64, Vec<i64>) -> TestResult);
    }

    #[test]
    fn insert_test() {
        let mut ckms = CKMS::<f64>::new(0.001);
        for i in 0..2 {
            ckms.insert(i as f64);
        }

        assert_eq!(0.0, ckms.samples[0].v);
        assert_eq!(1.0, ckms.samples[1].v);
    }


    // prop: v_i-1 < v_i =< v_i+1
    #[test]
    fn asc_samples_test() {
        fn asc_samples(fs: Vec<i64>) -> TestResult {
            let mut ckms = CKMS::<i64>::new(0.001);
            let fsc = fs.clone();
            for f in fs {
                ckms.insert(f);
            }

            if ckms.samples.len() == 0 && fsc.len() == 0 {
                return TestResult::passed();
            }
            let mut cur = ckms.samples[0].v;
            for ent in ckms.samples {
                let s = ent.v;
                if s < cur {
                    return TestResult::failed();
                }
                cur = s;
            }
            TestResult::passed()
        }
        QuickCheck::new()
            .tests(10000)
            .max_tests(100000)
            .quickcheck(asc_samples as fn(Vec<i64>) -> TestResult);
    }

    // prop: forall i. g_i + delta_i =< f(r_i, n)
    #[test]
    fn f_invariant_test() {
        fn f_invariant(fs: Vec<i64>) -> TestResult {
            let mut ckms = CKMS::<i64>::new(0.001);
            for f in fs {
                ckms.insert(f);
            }

            let s = ckms.samples.len();
            let mut r = 0;
            for i in 1..s {
                let ref prev = ckms.samples[i - 1];
                let ref cur = ckms.samples[i];

                r += prev.g;

                let res = (cur.g + cur.delta) <= ckms.invariant(r as f64);
                if !res {
                    println!("{:?} <= {:?}", cur.g + cur.delta, ckms.invariant(r as f64));
                    println!("samples: {:?}", ckms.samples);
                    return TestResult::failed();
                }
            }
            TestResult::passed()
        }
        QuickCheck::new()
            .tests(10000)
            .max_tests(100000)
            .quickcheck(f_invariant as fn(Vec<i64>) -> TestResult);
    }

    #[test]
    fn compression_test() {
        let mut ckms = CKMS::<i64>::new(0.1);
        for i in 1..10000 {
            ckms.insert(i);
        }
        ckms.compress();

        let l = ckms.samples.len();
        let n = ckms.count();
        assert_eq!(9999, n);
        assert_eq!(316, l);
    }

    // prop: post-compression, samples is bounded above by O(1/e log^2 en)
    #[test]
    fn compression_bound_test() {
        fn compression_bound(fs: Vec<i64>) -> TestResult {
            if fs.len() < 15 {
                return TestResult::discard();
            }

            let mut ckms = CKMS::<i64>::new(0.001);
            for f in fs {
                ckms.insert(f);
            }
            ckms.compress();

            let s = ckms.samples.len() as f64;
            let bound = (1.0 / ckms.error) * (ckms.error * (ckms.count() as f64)).log10().powi(2);

            if !(s <= bound) {
                println!("error: {:?} n: {:?} log10: {:?}",
                         ckms.error,
                         ckms.count() as f64,
                         (ckms.error * (ckms.count() as f64)).log10().powi(2));
                println!("{:?} <= {:?}", s, bound);
                return TestResult::failed();
            }
            TestResult::passed()
        }
        QuickCheck::new()
            .tests(10000)
            .max_tests(100000)
            .quickcheck(compression_bound as fn(Vec<i64>) -> TestResult);
    }

    #[test]
    fn test_basics() {
        let mut ckms = CKMS::<i32>::new(0.001);
        for i in 1..1001 {
            ckms.insert(i as i32);
        }

        assert_eq!(ckms.query(0.00), Some((1, 1)));
        assert_eq!(ckms.query(0.05), Some((50, 50)));
        assert_eq!(ckms.query(0.10), Some((100, 100)));
        assert_eq!(ckms.query(0.15), Some((150, 150)));
        assert_eq!(ckms.query(0.20), Some((200, 200)));
        assert_eq!(ckms.query(0.25), Some((250, 250)));
        assert_eq!(ckms.query(0.30), Some((300, 300)));
        assert_eq!(ckms.query(0.35), Some((350, 350)));
        assert_eq!(ckms.query(0.40), Some((400, 400)));
        assert_eq!(ckms.query(0.45), Some((450, 450)));
        assert_eq!(ckms.query(0.50), Some((500, 500)));
        assert_eq!(ckms.query(0.55), Some((550, 550)));
        assert_eq!(ckms.query(0.60), Some((600, 600)));
        assert_eq!(ckms.query(0.65), Some((650, 650)));
        assert_eq!(ckms.query(0.70), Some((700, 700)));
        assert_eq!(ckms.query(0.75), Some((750, 750)));
        assert_eq!(ckms.query(0.80), Some((800, 800)));
        assert_eq!(ckms.query(0.85), Some((850, 850)));
        assert_eq!(ckms.query(0.90), Some((900, 900)));
        assert_eq!(ckms.query(0.95), Some((950, 950)));
        assert_eq!(ckms.query(0.99), Some((990, 990)));
        assert_eq!(ckms.query(1.00), Some((1000, 1000)));
    }

    #[test]
    fn test_basics_float() {
        let mut ckms = CKMS::<f64>::new(0.001);
        for i in 1..1001 {
            ckms.insert(i as f64);
        }

        assert_eq!(ckms.query(0.00), Some((1, 1.0)));
        assert_eq!(ckms.query(1.00), Some((1000, 1000.0)));
    }
}

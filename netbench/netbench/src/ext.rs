use core::time::Duration;

pub trait ByteExt {
    fn bytes(&self) -> u64;
    fn kilobytes(&self) -> u64 {
        self.bytes() * 1000
    }
    fn megabytes(&self) -> u64 {
        self.kilobytes() * 1000
    }
    fn gigabytes(&self) -> u64 {
        self.megabytes() * 1000
    }
}

impl ByteExt for i32 {
    fn bytes(&self) -> u64 {
        *self as _
    }
}

impl ByteExt for u64 {
    fn bytes(&self) -> u64 {
        *self
    }
}

pub trait DurationExt {
    fn millis(&self) -> Duration;
    fn seconds(&self) -> Duration {
        self.millis() * 1000
    }
    fn minutes(&self) -> Duration {
        self.seconds() * 60
    }
}

impl DurationExt for i32 {
    fn millis(&self) -> Duration {
        Duration::from_millis(*self as _)
    }
}

impl DurationExt for u64 {
    fn millis(&self) -> Duration {
        Duration::from_millis(*self)
    }
}

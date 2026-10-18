//! Multi-planar V4L2 capture (`V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE`).
//!
//! Some SoC camera subsystems (e.g. Qualcomm CAMSS) expose their capture
//! nodes only through the multi-planar API, which the `v4l` crate implements
//! for neither formats nor streaming. Every format facelock decodes is
//! single-plane, so this handles exactly one plane per buffer and rejects
//! anything else.

use std::io;
use std::ptr::NonNull;
use std::sync::Arc;
use std::time::Duration;

use v4l::capability::{Capabilities, Flags};
use v4l::device::Handle;
use v4l::v4l_sys::{
    v4l2_buf_type_V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE, v4l2_buffer, v4l2_fmtdesc, v4l2_format,
    v4l2_memory_V4L2_MEMORY_MMAP, v4l2_plane, v4l2_requestbuffers,
};
use v4l::v4l2;
use v4l::{Device, FourCC};

const BUF_TYPE: u32 = v4l2_buf_type_V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE;
const MEMORY_MMAP: u32 = v4l2_memory_V4L2_MEMORY_MMAP;

/// Whether a device can only be captured through the multi-planar API.
pub fn is_mplane_only(caps: &Capabilities) -> bool {
    caps.capabilities.contains(Flags::VIDEO_CAPTURE_MPLANE)
        && !caps.capabilities.contains(Flags::VIDEO_CAPTURE)
}

/// A single-plane format as negotiated on a multi-planar node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Format {
    pub width: u32,
    pub height: u32,
    pub fourcc: FourCC,
    /// Bytes per row as delivered, including any padding.
    pub stride: u32,
}

/// A pixel format a multi-planar node advertises.
#[derive(Debug, Clone)]
pub struct FormatDesc {
    pub fourcc: FourCC,
    pub description: String,
}

/// Enumerate the pixel formats of a multi-planar capture node.
pub fn enum_formats(dev: &Device) -> io::Result<Vec<FormatDesc>> {
    let mut formats = Vec::new();
    for index in 0.. {
        let mut desc = v4l2_fmtdesc {
            index,
            type_: BUF_TYPE,
            ..unsafe { std::mem::zeroed() }
        };
        // SAFETY: `desc` is a valid, initialized v4l2_fmtdesc for this ioctl.
        let ret = unsafe {
            v4l2::ioctl(
                dev.handle().fd(),
                v4l2::vidioc::VIDIOC_ENUM_FMT,
                &mut desc as *mut _ as *mut std::os::raw::c_void,
            )
        };
        match ret {
            Ok(()) => formats.push(FormatDesc {
                fourcc: FourCC::from(desc.pixelformat),
                description: c_str(&desc.description),
            }),
            // EINVAL past the last index ends the enumeration.
            Err(e) if e.raw_os_error() == Some(libc::EINVAL) => break,
            Err(e) => return Err(e),
        }
    }
    Ok(formats)
}

/// The node's current format.
pub fn get_format(dev: &Device) -> io::Result<Format> {
    let mut fmt = v4l2_format {
        type_: BUF_TYPE,
        ..unsafe { std::mem::zeroed() }
    };
    // SAFETY: `fmt` is a valid v4l2_format with its type set.
    unsafe {
        v4l2::ioctl(
            dev.handle().fd(),
            v4l2::vidioc::VIDIOC_G_FMT,
            &mut fmt as *mut _ as *mut std::os::raw::c_void,
        )?;
    }
    from_v4l2(&fmt)
}

/// Request a format; returns what the driver actually set.
pub fn set_format(dev: &Device, fourcc: FourCC, width: u32, height: u32) -> io::Result<Format> {
    let mut fmt = v4l2_format {
        type_: BUF_TYPE,
        ..unsafe { std::mem::zeroed() }
    };
    // SAFETY: writing the pix_mp member of a zeroed union, which is the
    // member the multi-planar buffer type selects.
    unsafe {
        let pix = &mut fmt.fmt.pix_mp;
        pix.width = width;
        pix.height = height;
        pix.pixelformat = u32::from(fourcc);
        pix.num_planes = 1;
        v4l2::ioctl(
            dev.handle().fd(),
            v4l2::vidioc::VIDIOC_S_FMT,
            &mut fmt as *mut _ as *mut std::os::raw::c_void,
        )?;
    }
    from_v4l2(&fmt)
}

fn from_v4l2(fmt: &v4l2_format) -> io::Result<Format> {
    // SAFETY: the multi-planar buffer type selects the pix_mp member.
    let pix = unsafe { fmt.fmt.pix_mp };
    if pix.num_planes != 1 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "{} planes; only single-plane formats are supported",
                pix.num_planes
            ),
        ));
    }
    Ok(Format {
        width: pix.width,
        height: pix.height,
        fourcc: FourCC::from(pix.pixelformat),
        stride: pix.plane_fmt[0].bytesperline,
    })
}

fn c_str(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// One mmap'd plane, owned exclusively by its stream.
struct Mapping {
    ptr: NonNull<u8>,
    len: usize,
}

/// An MMAP capture stream on a multi-planar node, one plane per buffer.
pub struct Stream {
    handle: Arc<Handle>,
    mappings: Vec<Mapping>,
    streaming: bool,
    /// The buffer handed out by the last `next_frame()`, re-queued by the next one.
    /// Cleared only once re-queued, so a failed dequeue never re-queues a
    /// buffer the kernel already holds.
    pending: Option<u32>,
    timeout_ms: i32,
}

// SAFETY: the mappings are private to the stream and only touched through
// `&mut self`; nothing else holds the pointers.
unsafe impl Send for Stream {}

impl Stream {
    /// Allocate and map `count` buffers; streaming starts on the first `next_frame()`.
    pub fn with_buffers(dev: &Device, count: u32, timeout: Duration) -> io::Result<Self> {
        let handle = dev.handle();
        let mut req = v4l2_requestbuffers {
            count,
            type_: BUF_TYPE,
            memory: MEMORY_MMAP,
            ..unsafe { std::mem::zeroed() }
        };
        // SAFETY: `req` is a valid v4l2_requestbuffers.
        unsafe {
            v4l2::ioctl(
                handle.fd(),
                v4l2::vidioc::VIDIOC_REQBUFS,
                &mut req as *mut _ as *mut std::os::raw::c_void,
            )?;
        }
        let mut stream = Self {
            handle,
            mappings: Vec::with_capacity(req.count as usize),
            streaming: false,
            pending: None,
            timeout_ms: i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX),
        };
        for index in 0..req.count {
            let mut plane: v4l2_plane = unsafe { std::mem::zeroed() };
            let mut buf = stream.buffer(index, &mut plane);
            // SAFETY: `buf` points at one valid plane, as `length = 1` says.
            unsafe {
                v4l2::ioctl(
                    stream.handle.fd(),
                    v4l2::vidioc::VIDIOC_QUERYBUF,
                    &mut buf as *mut _ as *mut std::os::raw::c_void,
                )?;
            }
            let len = plane.length as usize;
            // SAFETY: mapping the plane at the offset the driver returned;
            // the mapping is released in Drop.
            let ptr = unsafe {
                v4l2::mmap(
                    std::ptr::null_mut(),
                    len,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_SHARED,
                    stream.handle.fd(),
                    libc::off_t::from(plane.m.mem_offset),
                )?
            };
            let ptr = NonNull::new(ptr.cast::<u8>())
                .ok_or_else(|| io::Error::other("mmap returned a null mapping"))?;
            stream.mappings.push(Mapping { ptr, len });
        }
        Ok(stream)
    }

    /// The next frame's bytes. Blocks up to the timeout.
    pub fn next_frame(&mut self) -> io::Result<&[u8]> {
        if !self.streaming {
            for index in 0..self.mappings.len() as u32 {
                self.queue(index)?;
            }
            self.ioctl_type(v4l2::vidioc::VIDIOC_STREAMON)?;
            self.streaming = true;
        } else if let Some(index) = self.pending {
            self.queue(index)?;
            self.pending = None;
        }

        if self.handle.poll(libc::POLLIN, self.timeout_ms)? == 0 {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "VIDIOC_DQBUF"));
        }
        let mut plane: v4l2_plane = unsafe { std::mem::zeroed() };
        let mut buf = self.buffer(0, &mut plane);
        // SAFETY: `buf` points at one valid plane for the driver to fill.
        unsafe {
            v4l2::ioctl(
                self.handle.fd(),
                v4l2::vidioc::VIDIOC_DQBUF,
                &mut buf as *mut _ as *mut std::os::raw::c_void,
            )?;
        }
        let mapping = self.mappings.get(buf.index as usize).ok_or_else(|| {
            io::Error::other(format!("driver returned unknown buffer {}", buf.index))
        })?;
        self.pending = Some(buf.index);
        let used = match plane.bytesused as usize {
            0 => mapping.len,
            n => n.min(mapping.len),
        };
        // SAFETY: the mapping is `len` bytes, dequeued (so the driver is not
        // writing it), and stays mapped while `&mut self` is borrowed.
        Ok(unsafe { std::slice::from_raw_parts(mapping.ptr.as_ptr(), used) })
    }

    fn buffer(&self, index: u32, plane: &mut v4l2_plane) -> v4l2_buffer {
        let mut buf = v4l2_buffer {
            index,
            type_: BUF_TYPE,
            memory: MEMORY_MMAP,
            length: 1,
            ..unsafe { std::mem::zeroed() }
        };
        buf.m.planes = plane;
        buf
    }

    fn queue(&mut self, index: u32) -> io::Result<()> {
        let mut plane: v4l2_plane = unsafe { std::mem::zeroed() };
        let mut buf = self.buffer(index, &mut plane);
        // SAFETY: `buf` points at one valid plane.
        unsafe {
            v4l2::ioctl(
                self.handle.fd(),
                v4l2::vidioc::VIDIOC_QBUF,
                &mut buf as *mut _ as *mut std::os::raw::c_void,
            )
        }
    }

    fn ioctl_type(&self, request: v4l2::vidioc::_IOC_TYPE) -> io::Result<()> {
        let mut typ = BUF_TYPE;
        // SAFETY: STREAMON/STREAMOFF take a pointer to the buffer type.
        unsafe {
            v4l2::ioctl(
                self.handle.fd(),
                request,
                &mut typ as *mut _ as *mut std::os::raw::c_void,
            )
        }
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        if self.streaming
            && let Err(e) = self.ioctl_type(v4l2::vidioc::VIDIOC_STREAMOFF)
        {
            tracing::warn!("multi-planar STREAMOFF failed: {e}");
        }
        for m in self.mappings.drain(..) {
            // SAFETY: each mapping was created by mmap with this length and
            // is unmapped exactly once.
            if let Err(e) = unsafe { v4l2::munmap(m.ptr.as_ptr().cast(), m.len) } {
                tracing::warn!("multi-planar munmap failed: {e}");
            }
        }
        // Release the buffers so the node can be reconfigured or reopened.
        let mut req = v4l2_requestbuffers {
            count: 0,
            type_: BUF_TYPE,
            memory: MEMORY_MMAP,
            ..unsafe { std::mem::zeroed() }
        };
        // SAFETY: `req` is a valid v4l2_requestbuffers.
        let _ = unsafe {
            v4l2::ioctl(
                self.handle.fd(),
                v4l2::vidioc::VIDIOC_REQBUFS,
                &mut req as *mut _ as *mut std::os::raw::c_void,
            )
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(bits: u32) -> Capabilities {
        let mut c: v4l::v4l_sys::v4l2_capability = unsafe { std::mem::zeroed() };
        // `Capabilities::from` reads the node's own capabilities.
        c.device_caps = bits;
        Capabilities::from(c)
    }

    #[test]
    fn only_mplane_only_nodes_take_the_mplane_path() {
        let single = Flags::VIDEO_CAPTURE.bits();
        let multi = Flags::VIDEO_CAPTURE_MPLANE.bits();
        assert!(is_mplane_only(&caps(multi)));
        assert!(!is_mplane_only(&caps(single)));
        assert!(!is_mplane_only(&caps(single | multi)));
        assert!(!is_mplane_only(&caps(0)));
    }

    #[test]
    fn single_plane_format_is_read_from_pix_mp() {
        let mut fmt: v4l2_format = unsafe { std::mem::zeroed() };
        fmt.type_ = BUF_TYPE;
        unsafe {
            fmt.fmt.pix_mp.width = 644;
            fmt.fmt.pix_mp.height = 604;
            fmt.fmt.pix_mp.pixelformat = u32::from(FourCC::new(b"GREY"));
            fmt.fmt.pix_mp.num_planes = 1;
            fmt.fmt.pix_mp.plane_fmt[0].bytesperline = 656;
        }
        let got = from_v4l2(&fmt).unwrap();
        assert_eq!(
            got,
            Format {
                width: 644,
                height: 604,
                fourcc: FourCC::new(b"GREY"),
                stride: 656
            }
        );
    }

    #[test]
    fn multi_plane_formats_are_refused() {
        let mut fmt: v4l2_format = unsafe { std::mem::zeroed() };
        fmt.fmt.pix_mp.num_planes = 2;
        assert!(from_v4l2(&fmt).is_err());
    }

    #[test]
    fn descriptions_stop_at_the_nul() {
        let mut raw = [0u8; 32];
        raw[..17].copy_from_slice(b"8-bit Greyscale\0x");
        assert_eq!(c_str(&raw), "8-bit Greyscale");
    }
}

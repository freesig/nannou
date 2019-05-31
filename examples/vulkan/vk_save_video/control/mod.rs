use nannou::prelude::*;
use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::Arc;
use std::thread::JoinHandle;

mod video;

// This is how many buffers the video saver will use
// The higher the number the less likely you applicaiton will
// slow down but the more behind your recording will be.
// So for live video this should be low.
pub const BUFFER_DEPTH: usize = 5;
// This must match the number of colours per
// pixel.
// RGBA = 4
// RGB = 3
// RG = 2 etc.
pub const NUM_COLOURS: usize = 4;

pub struct VideoControl {
    playing: bool,
    close_tx: Sender<()>,
    join_video: JoinHandle<()>,
    video_control: video::Control,
    video_buffer_out: SyncSender<Arc<vk::CpuAccessibleBuffer<[[u8; NUM_COLOURS]]>>>,
    video_buffer_in: Receiver<Arc<vk::CpuAccessibleBuffer<[[u8; NUM_COLOURS]]>>>,
}

pub fn new<P>(
    dimensions: (usize, usize),
    output_file: P,
    device: Arc<vk::Device>,
    frame_rate: usize,
) -> VideoControl
where
    P: AsRef<Path>,
{
    let buf = vec![[0u8; NUM_COLOURS]; dimensions.0 * dimensions.1];

    let (close_tx, close_stream) = mpsc::channel();
    let (video_buffer_out, next_frame) =
        mpsc::sync_channel::<Arc<vk::CpuAccessibleBuffer<[[u8; NUM_COLOURS]]>>>(BUFFER_DEPTH);
    let (frame_return, video_buffer_in) = mpsc::sync_channel(BUFFER_DEPTH);
    for _ in 0..BUFFER_DEPTH {
        let buf = buf.clone();
        let screenshot_buffer = vk::CpuAccessibleBuffer::from_iter(
            device.clone(),
            vk::BufferUsage::transfer_destination(),
            buf.into_iter(),
        )
        .expect("Failed to create screenshot buffer");
        frame_return
            .send(screenshot_buffer)
            .expect("Failed to initialize video buffers");
    }
    let cb = {
        // Frame number
        let mut i = 0;
        move |buf: &mut [u8]| {
            dbg!(i);
            if let Ok(frame) = next_frame.recv() {
                {
                    let buffer = loop {
                        if let Ok(buffer) = frame.read() {
                            break buffer;
                        }
                    };
                    for (b, t) in buf.chunks_exact_mut(4).zip(buffer.iter()) {
                        b.copy_from_slice(&t[..]);
                    }
                }
                frame_return.send(frame).ok();
            }
            dbg!(i);
            i += 1;
            i
        }
    };
    let vid = video::setup(
        cb,
        output_file,
        close_stream,
        dimensions.0 as u32,
        dimensions.1 as u32,
        frame_rate,
    )
    .expect("Failed to setup video");
    let video_control = vid.control();
    let join_video = std::thread::spawn(move || {
        video::run(vid).expect("Failed to run video");
    });
    VideoControl {
        playing: false,
        video_control,
        join_video,
        close_tx,
        video_buffer_out,
        video_buffer_in,
    }
}

impl VideoControl {
    pub fn play(&mut self) {
        if !self.playing {
            self.video_control.play();
            self.playing = true;
        }
    }
    pub fn stop(&mut self) {
        self.playing = false;
        self.close_tx.send(()).ok();
    }
    pub fn next_buffer(&self) -> Option<Arc<vk::CpuAccessibleBuffer<[[u8; NUM_COLOURS]]>>> {
        if self.playing {
            self.video_buffer_in.recv().ok()
        } else {
            None
        }
    }
    pub fn return_buffer(&self, buffer: Arc<vk::CpuAccessibleBuffer<[[u8; NUM_COLOURS]]>>) {
        self.video_buffer_out.send(buffer).ok();
    }
    pub fn close(mut self) {
        self.stop();
        self.join_video.join().expect("Failed to join video thread");
    }
}

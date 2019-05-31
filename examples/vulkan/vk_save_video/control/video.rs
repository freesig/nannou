// This example demonstrates the use of the encodebin element.
// The example takes an arbitrary URI as input, which it will try to decode
// and finally reencode using the encodebin element.
// For more information about how the decodebin element works, have a look at
// the decodebin-example.
// Since we tell the encodebin what format we want to get out of it from the start,
// it provides the correct caps and we can link it before starting the pipeline.
// After the decodebin has found all streams and we piped them into the encodebin,
// the operated pipeline looks as follows:

//                  /-{queue}-{audioconvert}-{audioresample}-\
// {uridecodebin} -|                                          {encodebin}-{filesink}
//                  \-{queue}-{videoconvert}-{videoscale}----/

use gstreamer as gst;
use gstreamer_app as gst_app;
use gstreamer_pbutils as gst_pbutils;
use gstreamer_video as gst_video;

use gst_pbutils::prelude::*;
use std::convert::TryInto;
use std::path::Path;
use std::sync::mpsc::Receiver;

struct MissingElement(&'static str);

pub struct Video {
    pipeline: gst::Pipeline,
}

pub struct Control {
    c: glib::WeakRef<gst::Pipeline>,
}

#[derive(Debug)]
pub struct Error;

impl Video {
    pub fn control(&self) -> Control {
        Control {
            c: self.pipeline.downgrade(),
        }
    }
}

impl Control {
    pub fn play(&self) {
        self.c.upgrade().map(|p| {
            p.set_state(gst::State::Playing).ok();
        });
    }
}

fn configure_encodebin(encodebin: &gst::Element) -> Result<(), Error> {
    // To tell the encodebin what we want it to produce, we create an EncodingProfile
    // https://gstreamer.freedesktop.org/data/doc/gstreamer/head/gst-plugins-base-libs/html/GstEncodingProfile.html
    // This profile consists of information about the contained audio and video formats
    // as well as the container format we want everything to be combined into.

    // Every audiostream piped into the encodebin should be encoded using vorbis.
    let audio_profile = gst_pbutils::EncodingAudioProfileBuilder::new()
        .format(&gst::Caps::new_simple("audio/x-vorbis", &[]))
        .presence(0)
        .build()
        .map_err(|_| Error)?;

    // Every videostream piped into the encodebin should be encoded using theora.
    let video_profile = gst_pbutils::EncodingVideoProfileBuilder::new()
        .format(&gst::Caps::new_simple("video/x-theora", &[]))
        .presence(0)
        .build()
        .map_err(|_| Error)?;

    // All streams are then finally combined into a matroska container.
    let container_profile = gst_pbutils::EncodingContainerProfileBuilder::new()
        .name("container")
        .format(&gst::Caps::new_simple("video/x-matroska", &[]))
        .add_profile(&(video_profile))
        .add_profile(&(audio_profile))
        .build()
        .map_err(|_| Error)?;

    // Finally, apply the EncodingProfile onto our encodebin element.
    encodebin
        .set_property("profile", &container_profile)
        .expect("set profile property failed");

    Ok(())
}

pub fn setup<F, P>(
    mut my_buffer: F,
    output_file: P,
    close_stream: Receiver<()>,
    width: u32,
    height: u32,
    frame_rate: usize,
) -> Result<Video, Error>
where
    F: FnMut(&mut [u8]) -> u64 + Send + 'static,
    P: AsRef<Path>,
{
    gst::init().map_err(|_| Error)?;

    let pipeline = gst::Pipeline::new(None);
    let src = gst::ElementFactory::make("appsrc", None)
        .ok_or(MissingElement("appsrc"))
        .map_err(|_| Error)?;
    let queue = gst::ElementFactory::make("queue", None)
        .ok_or(MissingElement("queue"))
        .map_err(|_| Error)?;
    let convert = gst::ElementFactory::make("videoconvert", None)
        .ok_or(MissingElement("videoconvert"))
        .map_err(|_| Error)?;
    let scale = gst::ElementFactory::make("videoscale", None)
        .ok_or(MissingElement("videoscale"))
        .map_err(|_| Error)?;
    let encodebin = gst::ElementFactory::make("encodebin", None)
        .ok_or(MissingElement("encodebin"))
        .map_err(|_| Error)?;
    let sink = gst::ElementFactory::make("filesink", None)
        .ok_or(MissingElement("filesink"))
        .map_err(|_| Error)?;

    sink.set_property(
        "location",
        &output_file
            .as_ref()
            .to_str()
            .expect("Failed to convert file path"),
    )
    .expect("setting location property failed");

    // Configure the encodebin.
    // Here we tell the bin what format we expect it to create at its output.
    configure_encodebin(&encodebin).map_err(|_| Error)?;

    pipeline
        .add_many(&[&src, &queue, &convert, &scale, &encodebin, &sink])
        .expect("failed to add elements to pipeline");
    // It is clear from the start, that encodebin has only one src pad, so we can
    // directly link it to our filesink without problems.
    // The caps of encodebin's src-pad are set after we configured the encoding-profile.
    // (But filesink doesn't really care about the caps at its input anyway)
    gst::Element::link_many(&[&src, &queue, &convert, &scale, &encodebin, &sink])
        .map_err(|_| Error)?;

    let appsrc = src
        .dynamic_cast::<gst_app::AppSrc>()
        .expect("Source element is expected to be an appsrc!");

    // Specify the format we want to provide as application into the pipeline
    // by creating a video info with the given format and creating caps from it for the appsrc element.
    let video_info = gst_video::VideoInfo::new(gst_video::VideoFormat::Bgrx, width, height)
        .fps(gst::Fraction::new(frame_rate.try_into().unwrap(), 2))
        .build()
        .expect("Failed to create video info");

    appsrc.set_caps(Some(&video_info.to_caps().unwrap()));
    appsrc.set_property_format(gst::Format::Time);

    appsrc.set_callbacks(
        // Since our appsrc element operates in pull mode (it asks us to provide data),
        // we add a handler for the need-data callback and provide new data from there.
        // In our case, we told gstreamer that we do 2 frames per second. While the
        // buffers of all elements of the pipeline are still empty, this will be called
        // a couple of times until all of them are filled. After this initial period,
        // this handler will be called (on average) twice per second.
        gst_app::AppSrcCallbacks::new()
            .need_data(move |appsrc, _| {
                if let Ok(_) = close_stream.try_recv() {
                    let _ = appsrc.end_of_stream();
                    return;
                }
                // Create the buffer that can hold exactly one BGRx frame.
                let mut buffer = gst::Buffer::with_size(video_info.size()).unwrap();
                {
                    let buffer = buffer.get_mut().unwrap();
                    // For each frame we produce, we set the timestamp when it should be displayed
                    // (pts = presentation time stamp)
                    // The autovideosink will use this information to display the frame at the right time.

                    // At this point, buffer is only a reference to an existing memory region somewhere.
                    // When we want to access its content, we have to map it while requesting the required
                    // mode of access (read, read/write).
                    // See: https://gstreamer.freedesktop.org/documentation/plugin-development/advanced/allocation.html
                    let i = {
                        let mut data = buffer.map_writable().unwrap();

                        my_buffer(data.as_mut_slice())
                    };
                    buffer.set_pts(i * (1.0 / frame_rate as f64) as u64 * gst::MSECOND);
                }

                // appsrc already handles the error here
                let _ = appsrc.push_buffer(buffer);
            })
            .build(),
    );

    Ok(Video { pipeline })
}

pub fn run(video: Video) -> Result<(), Error> {
    let pipeline = video.pipeline;
    pipeline.set_state(gst::State::Paused).map_err(|_| Error)?;
    let bus = pipeline
        .get_bus()
        .expect("Pipeline without bus. Shouldn't happen!");

    for msg in bus.iter_timed(gst::CLOCK_TIME_NONE) {
        use gst::MessageView;
        dbg!(&msg);
        if let Some(s) = msg.get_src() {
            eprintln!("{}", String::from(s.get_path_string()));
        }

        match msg.view() {
            MessageView::Eos(..) => {
                dbg!("EOS");
                break;
            }
            MessageView::Error(_) => {
                pipeline.set_state(gst::State::Null).map_err(|_| Error)?;
                break;
            }
            _ => (),
        }
    }
    dbg!("made it");

    pipeline.set_state(gst::State::Null).map_err(|_| Error)?;
    dbg!("made it");

    Ok(())
}

use std::{io::Cursor, sync::RwLock};

use lazy_static::lazy_static;
use reqwest::blocking::Client;
use symphonia::{core::{io::{MediaSource, MediaSourceStream}, formats::FormatOptions, meta::MetadataOptions, probe::Hint, audio::Channels}, default};
use cpal::{Stream, traits::{HostTrait, DeviceTrait, StreamTrait}, Device, StreamConfig};
use rb::{Producer, SpscRb, RB, RbConsumer, RbProducer};
use symphonia::core::audio::{SignalSpec, SampleBuffer, AudioBufferRef};

lazy_static!
{
    pub static ref BUFFER:RwLock<SpscRb<f32>> = RwLock::new(SpscRb::new(0));
}

#[cfg_attr(target_os = "android", ndk_glue::main(backtrace = "on"))]
pub fn main()
{
    let url = "https://ia800503.us.archive.org/8/items/futuresoundfx-98/futuresoundfx-1.mp3?cnt=0";
    
    let response = Client::new().get(url.clone())
        .header("Range", "bytes=0-")
        .send()
        .expect(format!("ERR: Could not open {url}").as_str());
        
    let source = Box::new(Cursor::new(response.bytes().unwrap().to_vec()));

    let (device, config) = CpalOutput::get_default_config();
    let stream = device.build_output_stream(
        &config,
        move |data:&mut [f32], _:&cpal::OutputCallbackInfo| {
            let written = BUFFER.read().unwrap().consumer().read(data).unwrap_or(0);
            
            data[written..].iter_mut().for_each(|s| *s = 0.0);
        },
        move |err| {
            panic!("ERR: An error occurred during the stream. {err}");
        }
    ).unwrap();

    loop
    {
        stream.play().unwrap();
        open(source.clone(), &stream);
    }
}

fn open(source:Box<dyn MediaSource>, stream:&Stream)
{
    let mss = MediaSourceStream::new(source, Default::default());

    let format_options = FormatOptions { enable_gapless: true, ..Default::default() };
    let metadata_options:MetadataOptions = Default::default();

    let mut reader = match default::get_probe().format(&Hint::new(), mss, &format_options, &metadata_options)
    {
        Err(err) => panic!("ERR: Failed to probe source. {err}"),
        Ok(probed) => probed.format
    };

    let track = reader.default_track().unwrap();
    let track_id = track.id;

    let mut decoder = default::get_codecs().make(&track.codec_params, &Default::default()).unwrap();
    let mut cpal_output:Option<CpalOutput> = None;

    loop
    {
        // Decode the next packet.
        let packet = match reader.next_packet()
        {
            Ok(packet) => packet,
            Err(_) => break
        };

        if packet.track_id() != track_id { continue; }

        match decoder.decode(&packet)
        {
            Err(err) => println!("WARN: Failed to decode sound. {err}"),
            Ok(decoded) => {
                if cpal_output.is_none()
                {
                    let spec = *decoded.spec();
                    let duration = decoded.capacity() as u64;
                    cpal_output.replace(CpalOutput::new());
                    cpal_output.as_mut().unwrap().update_buffers(spec, duration);
                }

                cpal_output.as_mut().unwrap().write(decoded);
            }
        }
    }

    stream.pause().unwrap();
}

//TODO: Support i16 and u16 instead of only f32.
pub struct CpalOutput
{
    _device:Device,
    _config:StreamConfig,
    _spec:SignalSpec,
    ring_buffer_writer:Producer<f32>,
    sample_buffer:SampleBuffer<f32>
}

impl CpalOutput
{
    fn get_default_config() -> (Device, StreamConfig)
    {
        let host = cpal::default_host();
        let device = host.default_output_device().expect("ERR: Failed to get default output device.");

        let channels = 1;
        let config = cpal::StreamConfig {
            channels: channels as cpal::ChannelCount,
            sample_rate: cpal::SampleRate(44100),
            buffer_size: cpal::BufferSize::Default,
        };

        (device, config)
    }

    pub fn update_buffers(&mut self, spec:SignalSpec, duration:u64)
    {
        let channels = spec.channels.count();
        let ring_len = ((200 * spec.rate as usize) / 1000) * channels;
        *BUFFER.write().unwrap() = SpscRb::new(ring_len);
        // Create the buffers for the stream.
        self.ring_buffer_writer = BUFFER.read().unwrap().producer();
        self.sample_buffer = SampleBuffer::<f32>::new(duration, spec);
    }

    /// Starts a new stream on the default device.
    /// Creates a new ring buffer and sample buffer.
    pub fn new() -> Self
    {
        let (device, config) = Self::get_default_config();
        let spec = SignalSpec { rate: 1, channels: Channels::all() };

        CpalOutput
        {
            _device: device,
            _config: config,
            _spec: spec,
            ring_buffer_writer: BUFFER.read().unwrap().producer(),
            sample_buffer: SampleBuffer::<f32>::new(0, spec)
        }
    }

    /// Write the `AudioBufferRef` to the buffers.
    pub fn write(&mut self, decoded:AudioBufferRef)
    {
        if decoded.frames() == 0 { return; }

        // CPAL wants the audio interleaved.
        self.sample_buffer.copy_interleaved_ref(decoded);
        let mut samples = self.sample_buffer.samples();
        
        // Write the interleaved samples to the ring buffer which is output by CPAL.
        while let Some(written) = self.ring_buffer_writer.write_blocking(samples)
        {
            samples = &samples[written..];
        }
    }
}
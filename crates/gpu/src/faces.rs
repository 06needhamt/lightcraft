//! Face networks on the GPU: an [`lightcraft_faces::net::Accelerator`] that runs the interpreter's convolutions (most of
//! YuNet's, SFace's and AuraFace's time) with a WGSL kernel (`wgsl/faces_conv.wgsl`). Everything else in the graph
//! stays on the CPU interpreter, which is the reference: a net attaches this only after it agreed with the CPU on a
//! test input (`Net::attach_checked`), and drops it on the first error.
//!
//! The device is a separate one (like denoise's), so a face scan and the interactive renders never share a queue, and
//! it respects the same switches as GPU rendering (`LIGHTCRAFT_GPU`, `LIGHTCRAFT_GPU_BACKEND`, the GPU preference)
//! and the crash sentinel around device creation. A model's weights are uploaded once, on first use, and stay on the
//! device while the accelerator (one per loaded model) lives. Every failure is an `Err`, never a panic.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

use bytemuck::{Pod, Zeroable};
use lightcraft_faces::net::{Accelerator, ConvJob};
use wgpu::util::DeviceExt;

/// Longest wait for one convolution before the device is given up on.
const TIMEOUT: Duration = Duration::from_secs(30);
/// Below this many multiply-adds a convolution stays on the CPU: the upload, dispatch and read-back cost more.
const MIN_MACS: usize = 1 << 20;
/// Workgroups a convolution should make at least, cutting its K into slices when its tiles are too few.
const TARGET_WORKGROUPS: usize = 256;
/// Weights one model may keep on the device (AuraFace's are about 250 MB).
const MAX_RESIDENT: u64 = 768 << 20;

struct Dev {
    device: wgpu::Device,
    queue: wgpu::Queue,
    info: wgpu::AdapterInfo,
    limits: wgpu::Limits,
    layout: wgpu::BindGroupLayout,
    tiled: wgpu::ComputePipeline,
    direct: wgpu::ComputePipeline,
    reduce: wgpu::ComputePipeline,
}

static DEV: OnceLock<Result<Dev, String>> = OnceLock::new();
static BROKEN: AtomicBool = AtomicBool::new(false);
static BROKEN_WHY: Mutex<Option<String>> = Mutex::new(None);

fn mark_broken(why: &str) {
    let mut w = BROKEN_WHY.lock().unwrap_or_else(PoisonError::into_inner);
    if w.is_none() {
        *w = Some(why.to_string());
    }
    BROKEN.store(true, Ordering::Relaxed);
}

/// Why the GPU stopped being used for faces (a device error), if it did.
pub fn broken_reason() -> Option<String> {
    BROKEN.load(Ordering::Relaxed).then(|| BROKEN_WHY.lock().unwrap_or_else(PoisonError::into_inner).clone().unwrap_or_else(|| "device error".into()))
}

fn dev() -> Result<&'static Dev, String> {
    if let Some(why) = broken_reason() {
        return Err(format!("stopped after a GPU failure: {why}"));
    }
    if let Some(why) = crate::switched_off() {
        return Err(why);
    }
    let _work = match DEV.get() {
        Some(_) => None,
        None => Some(crate::exit::enter().ok_or("the app is closing")?),
    };
    DEV.get_or_init(create_device).as_ref().map_err(|e| e.clone())
}

fn create_device() -> Result<Dev, String> {
    let Some(backends) = crate::backend::compute_backends() else { return Err("disabled by LIGHTCRAFT_GPU_BACKEND=off".into()) };
    crate::backend::with_init_marker(backends, || {
        std::panic::catch_unwind(|| make_device(backends)).unwrap_or_else(|_| Err("device creation panicked".into()))
    })
}

fn entry(binding: u32, ty: wgpu::BufferBindingType) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer { ty, has_dynamic_offset: false, min_binding_size: None },
        count: None,
    }
}

fn make_device(backends: wgpu::Backends) -> Result<Dev, String> {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = backends;
    desc.backend_options = crate::backend::backend_options();
    desc.flags = crate::backend::instance_flags_from_env(desc.flags);
    let instance = wgpu::Instance::new(desc);
    let wanted = std::env::var("LIGHTCRAFT_GPU_ADAPTER").ok().map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty());
    let adapter = match &wanted {
        Some(w) => pollster::block_on(instance.enumerate_adapters(backends))
            .into_iter()
            .find(|a| {
                let i = a.get_info();
                i.device_type != wgpu::DeviceType::Cpu && i.name.to_lowercase().contains(w.as_str())
            })
            .ok_or_else(|| format!("no GPU adapter named like `{w}`"))?,
        None => pollster::block_on(
            instance.request_adapter(&wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::HighPerformance, ..Default::default() }),
        )
        .map_err(|e| format!("no GPU adapter among {backends:?} ({e})"))?,
    };
    let info = adapter.get_info();
    if info.device_type == wgpu::DeviceType::Cpu {
        return Err(format!("software adapter ({}) skipped: the CPU is faster", info.name));
    }
    let limits = adapter.limits();
    if limits.max_storage_buffers_per_shader_stage < 4
        || limits.max_compute_invocations_per_workgroup < 256
        || limits.max_compute_workgroup_size_x < 256
        || limits.max_compute_workgroup_storage_size < 8 * 1024
    {
        return Err(format!("{} is too limited for the face kernels", info.name));
    }
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("lightcraft-faces"),
        required_limits: limits.clone(),
        ..Default::default()
    }))
    .map_err(|e| format!("{}: device creation failed ({e})", info.name))?;
    device.on_uncaptured_error(Arc::new(|e| {
        log::error!("gpu (faces): {e}");
        mark_broken(&format!("device error: {e}"));
    }));
    device.set_device_lost_callback(|reason, msg| {
        log::error!("gpu (faces): device lost ({reason:?}): {msg}");
        mark_broken(&format!("device lost ({reason:?}): {msg}"));
    });
    use wgpu::BufferBindingType::{Storage, Uniform};
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("faces conv"),
        entries: &[
            entry(0, Uniform),
            entry(1, Storage { read_only: true }),
            entry(2, Storage { read_only: true }),
            entry(3, Storage { read_only: true }),
            entry(4, Storage { read_only: false }),
        ],
    });
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("faces conv"),
        source: wgpu::ShaderSource::Wgsl(include_str!("wgsl/faces_conv.wgsl").into()),
    });
    let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[Some(&layout)], immediate_size: 0 });
    let pipe = |entry: &str| {
        device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some(entry),
            layout: Some(&pl),
            module: &module,
            entry_point: Some(entry),
            // both kernels write all of their workgroup memory before reading it
            compilation_options: wgpu::PipelineCompilationOptions { zero_initialize_workgroup_memory: false, ..Default::default() },
            cache: None,
        })
    };
    let (tiled, direct, reduce) = (pipe("tiled"), pipe("direct"), pipe("reduce"));
    log::info!("gpu (faces): {} ({:?})", info.name, info.backend);
    Ok(Dev { device, queue, info, limits, layout, tiled, direct, reduce })
}

/// The adapter face scans would use ("AMD Radeon RX 7900 XTX (Vulkan)"), or why they cannot use one. Creates the
/// device on first use.
pub fn adapter() -> Result<String, String> {
    dev().map(|d| format!("{} ({:?})", d.info.name, d.info.backend))
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params {
    m: u32,
    n: u32,
    k: u32,
    cg: u32,
    h: u32,
    w: u32,
    ow: u32,
    kh: u32,
    kw: u32,
    sh: u32,
    sw: u32,
    pt: u32,
    pl: u32,
    bias: u32,
    total: u32,
    row: u32,
    splits: u32,
    kchunk: u32,
    _a: u32,
    _b: u32,
}

/// A layer's weights and bias on the device.
struct Resident {
    weights: wgpu::Buffer,
    bias: wgpu::Buffer,
}

/// The GPU accelerator for one loaded face model: its weights stay on the device while it lives.
pub struct FaceGpu {
    dev: &'static Dev,
    layers: Mutex<HashMap<usize, Arc<Resident>>>,
    resident: AtomicU64,
}

/// A GPU accelerator for one face model, or why there is none (GPU off, missing or broken).
pub fn accelerator() -> Result<Arc<FaceGpu>, String> {
    Ok(Arc::new(FaceGpu { dev: dev()?, layers: Mutex::new(HashMap::new()), resident: AtomicU64::new(0) }))
}

fn u32_of(v: usize) -> Result<u32, String> {
    u32::try_from(v).ok().filter(|v| *v < (1 << 31)).ok_or_else(|| "a layer too large for the GPU kernel".to_string())
}

impl FaceGpu {
    /// Bytes of weights this model keeps on the device.
    pub fn resident_bytes(&self) -> u64 {
        self.resident.load(Ordering::Relaxed)
    }

    fn resident(&self, job: &ConvJob) -> Result<Option<Arc<Resident>>, String> {
        let mut layers = self.layers.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(r) = layers.get(&job.layer) {
            return Ok(Some(r.clone()));
        }
        let bytes = (job.weights.len() as u64 + job.cout as u64) * 4;
        if self.resident_bytes().saturating_add(bytes) > MAX_RESIDENT {
            return Ok(None);
        }
        let d = self.dev;
        let usage = wgpu::BufferUsages::STORAGE;
        let weights = d.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("faces weights"),
            contents: bytemuck::cast_slice(job.weights),
            usage,
        });
        let zero = [0f32; 1];
        let bias = d.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("faces bias"),
            contents: bytemuck::cast_slice(job.bias.unwrap_or(&zero)),
            usage,
        });
        let r = Arc::new(Resident { weights, bias });
        self.resident.fetch_add(bytes, Ordering::Relaxed);
        layers.insert(job.layer, r.clone());
        Ok(Some(r))
    }

    fn run(&self, job: &ConvJob) -> Result<Option<Vec<f32>>, String> {
        let d = self.dev;
        let groups = job.groups.max(1);
        let (m, cg) = (job.cout / groups, job.cin / groups);
        let n = job.oh.checked_mul(job.ow).ok_or("a layer too large")?;
        let k = cg.checked_mul(job.kh).and_then(|v| v.checked_mul(job.kw)).ok_or("a layer too large")?;
        let total = job.cout.checked_mul(n).ok_or("a layer too large")?;
        let direct = m < 8;
        // Too few 64 x 64 tiles to keep the GPU busy (a 7 x 7 map of 512 channels makes 8): cut K into slices that
        // run side by side, and sum them in a second pass.
        let tiles = n.div_ceil(64) * m.div_ceil(64) * groups;
        let splits = if direct { 1 } else { (TARGET_WORKGROUPS / tiles.max(1)).clamp(1, (k / 128).max(1)) };
        let kchunk = k.div_ceil(splits).div_ceil(16) * 16;
        let splits = k.div_ceil(kchunk.max(1)).max(1);
        let partial = total.checked_mul(splits).ok_or("a layer too large")?;
        let largest = [job.input.len(), job.weights.len(), partial].into_iter().max().unwrap_or(0) as u64 * 4;
        if largest > d.limits.max_storage_buffer_binding_size.min(d.limits.max_buffer_size) {
            return Ok(None);
        }
        let flat = |count: usize| {
            let blocks = count.div_ceil(256);
            let row = blocks.clamp(1, 65_535);
            (row, blocks.div_ceil(row), 1, row)
        };
        let (wx, wy, wz, row) = if direct { flat(total) } else { (n.div_ceil(64), m.div_ceil(64), groups * splits, flat(total).3) };
        let (rx, ry, _, _) = flat(total);
        let limit = d.limits.max_compute_workgroups_per_dimension as usize;
        if [wx, wy, wz, rx, ry].into_iter().any(|v| v > limit) {
            return Ok(None);
        }
        let Some(res) = self.resident(job)? else { return Ok(None) };
        let params = Params {
            m: u32_of(m)?,
            n: u32_of(n)?,
            k: u32_of(k)?,
            cg: u32_of(cg)?,
            h: u32_of(job.h)?,
            w: u32_of(job.w)?,
            ow: u32_of(job.ow)?,
            kh: u32_of(job.kh)?,
            kw: u32_of(job.kw)?,
            sh: u32_of(job.sh)?,
            sw: u32_of(job.sw)?,
            pt: u32_of(job.pt)?,
            pl: u32_of(job.pl)?,
            bias: u32::from(job.bias.is_some()),
            total: u32_of(total)?,
            row: u32_of(row)?,
            splits: u32_of(splits)?,
            kchunk: u32_of(kchunk)?,
            _a: 0,
            _b: 0,
        };
        let Some(_work) = crate::exit::enter() else { return Err("the app is closing".into()) };
        let scopes = [d.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory), d.device.push_error_scope(wgpu::ErrorFilter::Validation)];
        let uniform = d.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("faces params"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let input = d.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("faces input"),
            contents: bytemuck::cast_slice(job.input),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let bytes = total as u64 * 4;
        let output = d.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("faces output"),
            size: bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let partials = (splits > 1).then(|| {
            d.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("faces partial sums"),
                size: partial as u64 * 4,
                usage: wgpu::BufferUsages::STORAGE,
                mapped_at_creation: false,
            })
        });
        let staging = d.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("faces read-back"),
            size: bytes,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind = d.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &d.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: input.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: res.weights.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: res.bias.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 4, resource: partials.as_ref().unwrap_or(&output).as_entire_binding() },
            ],
        });
        let summing = partials.as_ref().map(|partials| {
            d.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &d.layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: partials.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 2, resource: res.weights.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 3, resource: res.bias.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 4, resource: output.as_entire_binding() },
                ],
            })
        });
        let mut enc = d.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("faces conv"), timestamp_writes: None });
            pass.set_pipeline(if direct { &d.direct } else { &d.tiled });
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups(u32_of(wx)?, u32_of(wy)?, u32_of(wz)?);
            if let Some(summing) = &summing {
                pass.set_pipeline(&d.reduce);
                pass.set_bind_group(0, summing, &[]);
                pass.dispatch_workgroups(u32_of(rx)?, u32_of(ry)?, 1);
            }
        }
        enc.copy_buffer_to_buffer(&output, 0, &staging, 0, bytes);
        let index = d.queue.submit([enc.finish()]);
        let slice = staging.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        let waited = d.device.poll(wgpu::PollType::Wait { submission_index: Some(index), timeout: Some(TIMEOUT) });
        let [oom, validation] = scopes;
        let (validation, oom) = (pollster::block_on(validation.pop()), pollster::block_on(oom.pop()));
        if let Some(e) = oom {
            return Err(format!("the GPU ran out of memory: {e}"));
        }
        if let Some(e) = validation {
            mark_broken(&format!("device error: {e}"));
            return Err(format!("the GPU reported an error: {e}"));
        }
        if let Err(e) = waited {
            mark_broken(&format!("the GPU did not finish a convolution: {e}"));
            return Err(format!("the GPU did not finish: {e}"));
        }
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(format!("reading the result back failed: {e}")),
            Err(e) => return Err(format!("reading the result back failed: {e}")),
        }
        let out = {
            let data = slice.get_mapped_range().map_err(|e| format!("reading the result back failed: {e:?}"))?;
            data.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect::<Vec<f32>>()
        };
        staging.unmap();
        Ok(Some(out))
    }
}

impl Accelerator for FaceGpu {
    fn device(&self) -> String {
        format!("{} ({:?})", self.dev.info.name, self.dev.info.backend)
    }

    fn conv(&self, job: &ConvJob) -> Result<Option<Vec<f32>>, String> {
        if let Some(why) = broken_reason() {
            return Err(format!("the GPU is not used any more: {why}"));
        }
        if crate::switched_off().is_some() || job.macs() < MIN_MACS {
            return Ok(None);
        }
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.run(job))).unwrap_or_else(|_| {
            mark_broken("a face convolution panicked");
            Err("the GPU runner gave up".into())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gpu() -> Option<Arc<FaceGpu>> {
        match accelerator() {
            Ok(a) => Some(a),
            Err(e) => {
                eprintln!("no GPU for the face kernels, skipping: {e}");
                None
            }
        }
    }

    /// The plain definition of a convolution, to check the kernels against.
    fn reference(j: &ConvJob) -> Vec<f32> {
        let (m, cg) = (j.cout / j.groups, j.cin / j.groups);
        let mut out = vec![0f32; j.cout * j.oh * j.ow];
        for co in 0..j.cout {
            let g = co / m;
            for oy in 0..j.oh {
                for ox in 0..j.ow {
                    let mut v = j.bias.map_or(0.0, |b| b[co]);
                    for ci in 0..cg {
                        for ky in 0..j.kh {
                            for kx in 0..j.kw {
                                let (iy, ix) = ((oy * j.sh + ky) as isize - j.pt as isize, (ox * j.sw + kx) as isize - j.pl as isize);
                                if iy >= 0 && ix >= 0 && (iy as usize) < j.h && (ix as usize) < j.w {
                                    v += j.weights[((co * cg + ci) * j.kh + ky) * j.kw + kx]
                                        * j.input[((g * cg + ci) * j.h + iy as usize) * j.w + ix as usize];
                                }
                            }
                        }
                    }
                    out[(co * j.oh + oy) * j.ow + ox] = v;
                }
            }
        }
        out
    }

    #[test]
    fn the_gpu_convolution_gives_what_the_definition_gives() {
        let Some(gpu) = gpu() else { return };
        // (cin, cout, groups, k, stride, pad, h, w): dense, strided, depthwise, grouped, pointwise, odd sizes
        for (layer, (cin, cout, groups, k, s, p, h, w)) in [
            (3, 16, 1, 3, 2, 1, 41, 37),
            (64, 64, 1, 3, 1, 1, 28, 28),
            (32, 32, 32, 3, 1, 1, 30, 31),
            (16, 32, 4, 3, 2, 1, 33, 20),
            (48, 72, 1, 1, 1, 0, 50, 45),
            (8, 130, 1, 5, 1, 2, 23, 19),
        ]
        .into_iter()
        .enumerate()
        {
            let cg = cin / groups;
            let weights: Vec<f32> = (0..cout * cg * k * k).map(|i| ((i * 37 % 23) as f32 - 11.0) * 0.03).collect();
            let bias: Vec<f32> = (0..cout).map(|i| i as f32 * 0.01 - 0.3).collect();
            let input: Vec<f32> = (0..cin * h * w).map(|i| ((i * 13 % 29) as f32 - 14.0) * 0.07).collect();
            let (oh, ow) = ((h + 2 * p - k) / s + 1, (w + 2 * p - k) / s + 1);
            let job = ConvJob {
                layer,
                cin,
                h,
                w,
                cout,
                groups,
                kh: k,
                kw: k,
                sh: s,
                sw: s,
                pt: p,
                pl: p,
                oh,
                ow,
                weights: &weights,
                bias: Some(&bias),
                input: &input,
            };
            let got = gpu.run(&job).unwrap().unwrap();
            let want = reference(&job);
            let worst = got.iter().zip(&want).fold(0f32, |m, (a, b)| m.max((a - b).abs()));
            assert!(worst < 1e-3, "case {layer}: off by {worst}");
        }
        assert!(gpu.resident_bytes() > 0, "the weights stay on the device");
    }

    /// The real models on the GPU agree with the CPU interpreter: `LC_FACE_MODELS=<folder>` holding
    /// `face_detection_yunet_2023mar.onnx`, `face_recognition_sface_2021dec.onnx` and (optionally) `glintr100.onnx`.
    #[test]
    fn real_face_models_on_the_gpu_agree_with_the_cpu() {
        let Some(dir) = std::env::var_os("LC_FACE_MODELS").map(std::path::PathBuf::from) else {
            eprintln!("LC_FACE_MODELS is not set: skipping");
            return;
        };
        if gpu().is_none() {
            return;
        }
        let det = lightcraft_faces::yunet::Detector::new(&std::fs::read(dir.join("face_detection_yunet_2023mar.onnx")).unwrap()).unwrap();
        let (w, h) = (900, 700);
        let rgb: Vec<u8> = (0..w * h * 3).map(|i| ((i / 3 % w) as f32 * 0.25 + (i / 3 / w) as f32 * 0.3 + (i * 7919 % 41) as f32) as u8).collect();
        let opts = lightcraft_faces::yunet::Options { score: 0.0, ..Default::default() };
        let cpu = det.detect(&rgb, w, h, &opts).unwrap();
        det.accelerate(accelerator().unwrap(), false).unwrap();
        let fast = det.detect(&rgb, w, h, &opts).unwrap();
        assert!(det.net().accelerator().is_some());
        assert_eq!(cpu.len(), fast.len());
        for (a, b) in cpu.iter().zip(&fast) {
            assert!((a.score - b.score).abs() < 1e-3, "{} vs {}", a.score, b.score);
        }
        for (file, sha) in [
            ("face_recognition_sface_2021dec.onnx", lightcraft_faces::known::SFACE_SHA256),
            ("glintr100.onnx", lightcraft_faces::known::AURAFACE_SHA256),
        ] {
            let path = dir.join(file);
            if !path.exists() {
                continue;
            }
            let e = lightcraft_faces::runtime::Embedder::load(&path, &lightcraft_faces::known::lookup(sha).unwrap()).unwrap();
            let (ew, eh) = e.input_size();
            let face: Vec<u8> = (0..ew * eh * 3).map(|i| ((i * 13) % 251) as u8).collect();
            let cpu = e.embed(&face).unwrap();
            e.accelerate(accelerator().unwrap(), false).unwrap();
            let fast = e.embed(&face).unwrap();
            let cosine: f32 = cpu.iter().zip(&fast).map(|(a, b)| a * b).sum();
            assert!(cosine > 0.9999, "{file}: cosine {cosine}");
        }
    }

    /// Timings, CPU against GPU: `LC_FACE_MODELS=<folder> cargo test --release -p lightcraft-gpu faces::tests::timings -- --ignored --nocapture`.
    #[test]
    #[ignore = "a benchmark: needs LC_FACE_MODELS and a GPU"]
    fn timings() {
        let Some(dir) = std::env::var_os("LC_FACE_MODELS").map(std::path::PathBuf::from) else { return };
        let Some(_) = gpu() else { return };
        let time = |runs: usize, f: &mut dyn FnMut()| {
            f();
            let t = std::time::Instant::now();
            for _ in 0..runs {
                f();
            }
            t.elapsed().as_secs_f64() * 1000.0 / runs as f64
        };
        let det = lightcraft_faces::yunet::Detector::new(&std::fs::read(dir.join("face_detection_yunet_2023mar.onnx")).unwrap()).unwrap();
        let (w, h) = (1024, 768);
        let rgb: Vec<u8> = (0..w * h * 3).map(|i| (i * 7 % 251) as u8).collect();
        let opts = lightcraft_faces::yunet::Options::default();
        let cpu = time(5, &mut || drop(det.detect(&rgb, w, h, &opts).unwrap()));
        det.accelerate(accelerator().unwrap(), false).unwrap();
        let fast = time(5, &mut || drop(det.detect(&rgb, w, h, &opts).unwrap()));
        eprintln!("YuNet 640: CPU {cpu:.1} ms, GPU {fast:.1} ms");
        for (file, sha) in [
            ("face_recognition_sface_2021dec.onnx", lightcraft_faces::known::SFACE_SHA256),
            ("glintr100.onnx", lightcraft_faces::known::AURAFACE_SHA256),
        ] {
            let e = lightcraft_faces::runtime::Embedder::load(&dir.join(file), &lightcraft_faces::known::lookup(sha).unwrap()).unwrap();
            let (ew, eh) = e.input_size();
            let face: Vec<u8> = (0..ew * eh * 3).map(|i| (i * 13 % 251) as u8).collect();
            let cpu = time(3, &mut || drop(e.embed(&face).unwrap()));
            e.accelerate(accelerator().unwrap(), false).unwrap();
            let fast = time(3, &mut || drop(e.embed(&face).unwrap()));
            eprintln!("{file}: CPU {cpu:.1} ms, GPU {fast:.1} ms");
        }
    }
}

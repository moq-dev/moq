//! The Direct3D12 route: render targets shared with a Direct3D11 device on the
//! same adapter, converted to NV12 there for Media Foundation.
//!
//! wgpu draws into a BGRA texture created shareable on its own D3D12 device, and
//! the D3D11 device opens the same memory. Once wgpu's copy lands, the D3D11
//! video processor converts it into an NV12 texture the encoder reads, the same
//! conversion a Windows.Graphics.Capture frame takes. A slot is drawn into again
//! only once that conversion has finished reading it.

use std::sync::{Arc, Mutex};

use windows::Win32::Foundation::{CloseHandle, GENERIC_ALL};
use windows::Win32::Graphics::Direct3D11::{
	D3D11_QUERY_DESC, D3D11_QUERY_EVENT, ID3D11Device, ID3D11Device1, ID3D11Query, ID3D11Texture2D,
};
use windows::Win32::Graphics::Direct3D12::{
	D3D12_CPU_PAGE_PROPERTY_UNKNOWN, D3D12_HEAP_FLAG_SHARED, D3D12_HEAP_PROPERTIES, D3D12_HEAP_TYPE_DEFAULT,
	D3D12_MEMORY_POOL_UNKNOWN, D3D12_RESOURCE_DESC, D3D12_RESOURCE_DIMENSION_TEXTURE2D,
	D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET, D3D12_RESOURCE_FLAG_ALLOW_SIMULTANEOUS_ACCESS,
	D3D12_RESOURCE_STATE_COMMON, D3D12_TEXTURE_LAYOUT_UNKNOWN, ID3D12Device, ID3D12Resource,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter, IDXGIFactory4};
use windows::core::{BOOL, Interface};

use super::{Blit, Exporter, err, extent, finish};
use crate::frame::d3d11::{self, Texture};
use crate::{Error, Size, Surface};

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8Unorm;

pub(super) struct Export {
	device: wgpu::Device,
	queue: wgpu::Queue,
	raw: ID3D12Device,
	/// The encoder's device, on wgpu's adapter.
	d3d11: ID3D11Device,
	size: Size,
	blit: Blit,
	pool: Arc<Mutex<Pool>>,
}

/// Slots not held by a [`Copy`].
#[derive(Default)]
struct Pool {
	free: Vec<Slot>,
	/// Slots in existence, up to [`Exporter::DEPTH`].
	allocated: usize,
}

/// One shared render target, seen by both devices.
struct Slot {
	view: wgpu::TextureView,
	shared: ID3D11Texture2D,
	/// Signals once the last conversion finished reading the slot.
	read: Option<ID3D11Query>,
}

impl Export {
	/// Fails when `device` is not a Direct3D12 device, `size` has an odd edge
	/// NV12 cannot hold, or the adapter cannot convert a shared texture to NV12.
	pub fn open(device: &wgpu::Device, queue: &wgpu::Queue, size: Size) -> Result<Self, Error> {
		// SAFETY: the raw device is retained before the guard drops.
		let Some(raw) = (unsafe { device.as_hal::<wgpu::hal::api::Dx12>() }).map(|hal| hal.raw_device().clone()) else {
			return Err(err(
				"it is not a Direct3D12 device; open wgpu on its Direct3D12 backend",
			));
		};
		if !size.width.is_multiple_of(2) || !size.height.is_multiple_of(2) {
			return Err(err(format!("NV12 cannot hold {size}, which has an odd edge")));
		}

		// Sharing a texture needs both devices on one adapter.
		let luid = unsafe { raw.GetAdapterLuid() };
		let factory: IDXGIFactory4 =
			unsafe { CreateDXGIFactory1() }.map_err(|e| err(format!("CreateDXGIFactory1: {e}")))?;
		let adapter: IDXGIAdapter =
			unsafe { factory.EnumAdapterByLuid(luid) }.map_err(|e| err(format!("EnumAdapterByLuid: {e}")))?;
		let d3d11 = d3d11::create_device(Some(&adapter))?;

		let export = Self {
			device: device.clone(),
			queue: queue.clone(),
			raw,
			d3d11,
			size,
			blit: Blit::new(device, FORMAT),
			pool: Arc::default(),
		};
		// Checked once here, so a driver without the video processor conversion
		// fails setup rather than the first copy.
		let slot = export.reserve()?.expect("an unused pool has room");
		if let Err(e) = Texture::capture(&export.d3d11, &slot.shared, size) {
			return Err(err(format!("its adapter cannot convert a shared texture to NV12: {e}")));
		}
		export.pool.lock().unwrap().free.push(slot);
		Ok(export)
	}

	pub fn copy(&mut self, texture: &wgpu::Texture) -> Result<Option<Copy>, Error> {
		let Some(slot) = self.reserve()? else {
			return Ok(None);
		};
		let submission = self.blit.submit(&self.device, &self.queue, texture, &slot.view);
		Ok(Some(Copy {
			slot: Some(slot),
			submission,
			device: self.d3d11.clone(),
			size: self.size,
			pool: self.pool.clone(),
		}))
	}

	/// A slot no conversion is still reading, or `None` when all
	/// [`Exporter::DEPTH`] are busy.
	fn reserve(&self) -> Result<Option<Slot>, Error> {
		let mut pool = self.pool.lock().unwrap();
		let context =
			unsafe { self.d3d11.GetImmediateContext() }.map_err(|e| err(format!("GetImmediateContext: {e}")))?;
		let idle = pool.free.iter().position(|slot| {
			slot.read.as_ref().is_none_or(|query| {
				// An event query writes TRUE once the GPU has passed it, and
				// nothing before.
				let mut done = BOOL(0);
				let size = std::mem::size_of::<BOOL>() as u32;
				let data = Some((&raw mut done).cast());
				unsafe { context.GetData(query, data, size, 0) }.is_ok() && done.as_bool()
			})
		});
		if let Some(index) = idle {
			return Ok(Some(pool.free.swap_remove(index)));
		}
		if pool.allocated == Exporter::DEPTH {
			return Ok(None);
		}
		let slot = self.allocate()?;
		pool.allocated += 1;
		Ok(Some(slot))
	}

	fn allocate(&self) -> Result<Slot, Error> {
		let heap = D3D12_HEAP_PROPERTIES {
			Type: D3D12_HEAP_TYPE_DEFAULT,
			CPUPageProperty: D3D12_CPU_PAGE_PROPERTY_UNKNOWN,
			MemoryPoolPreference: D3D12_MEMORY_POOL_UNKNOWN,
			CreationNodeMask: 0,
			VisibleNodeMask: 0,
		};
		let desc = D3D12_RESOURCE_DESC {
			Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
			Alignment: 0,
			Width: u64::from(self.size.width),
			Height: self.size.height,
			DepthOrArraySize: 1,
			MipLevels: 1,
			Format: DXGI_FORMAT_B8G8R8A8_UNORM,
			SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
			Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
			// Simultaneous access lets the D3D11 device read it with no state
			// transition, once wgpu's write has finished.
			Flags: D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET | D3D12_RESOURCE_FLAG_ALLOW_SIMULTANEOUS_ACCESS,
		};
		let mut resource: Option<ID3D12Resource> = None;
		unsafe {
			self.raw.CreateCommittedResource(
				&heap,
				D3D12_HEAP_FLAG_SHARED,
				&desc,
				D3D12_RESOURCE_STATE_COMMON,
				None,
				&mut resource,
			)
		}
		.map_err(|e| err(format!("CreateCommittedResource: {e}")))?;
		let resource = resource.ok_or_else(|| err("CreateCommittedResource returned null"))?;

		let handle = unsafe { self.raw.CreateSharedHandle(&resource, None, GENERIC_ALL.0, None) }
			.map_err(|e| err(format!("CreateSharedHandle: {e}")))?;
		let shared = self
			.d3d11
			.cast::<ID3D11Device1>()
			.and_then(|device| unsafe { device.OpenSharedResource1::<ID3D11Texture2D>(handle) });
		// The D3D11 texture holds the memory open; the handle only carried it over.
		let _ = unsafe { CloseHandle(handle) };
		let shared = shared.map_err(|e| err(format!("OpenSharedResource1: {e}")))?;

		let descriptor = wgpu::TextureDescriptor {
			label: Some("moq-video export"),
			size: extent(self.size),
			mip_level_count: 1,
			sample_count: 1,
			dimension: wgpu::TextureDimension::D2,
			format: FORMAT,
			usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
			view_formats: &[],
		};
		// SAFETY: the resource is a 2D, single-level, single-sample BGRA8 texture
		// of this extent on wgpu's own device, allowed as a render target, and in
		// the common state wgpu takes an uninitialized texture to be in.
		let texture = unsafe {
			let hal = wgpu::hal::dx12::Device::texture_from_raw(
				resource,
				FORMAT,
				wgpu::TextureDimension::D2,
				extent(self.size),
				1,
				1,
			);
			self.device.create_texture_from_hal::<wgpu::hal::api::Dx12>(
				hal,
				&descriptor,
				wgpu::TextureUses::UNINITIALIZED,
			)
		};
		Ok(Slot {
			view: texture.create_view(&Default::default()),
			shared,
			read: None,
		})
	}
}

/// One copy in flight. Dropping it gives its slot back.
pub(super) struct Copy {
	slot: Option<Slot>,
	submission: wgpu::SubmissionIndex,
	device: ID3D11Device,
	size: Size,
	pool: Arc<Mutex<Pool>>,
}

impl Copy {
	pub fn wait(mut self, device: &wgpu::Device) -> Result<Surface, Error> {
		// Created up front, so no failure leaves a conversion reading the slot
		// with nothing to say when it ends.
		let desc = D3D11_QUERY_DESC {
			Query: D3D11_QUERY_EVENT,
			MiscFlags: 0,
		};
		let mut query: Option<ID3D11Query> = None;
		unsafe { self.device.CreateQuery(&desc, Some(&mut query)) }.map_err(|e| err(format!("CreateQuery: {e}")))?;
		let query = query.ok_or_else(|| err("CreateQuery returned null"))?;
		let context =
			unsafe { self.device.GetImmediateContext() }.map_err(|e| err(format!("GetImmediateContext: {e}")))?;

		finish(device, self.submission.clone())?;
		let slot = self.slot.as_mut().expect("held until drop");
		let texture = Texture::capture(&self.device, &slot.shared, self.size)?;
		unsafe {
			context.End(&query);
			// Submits the conversion now rather than whenever the context next
			// fills, so the query comes true without anyone else's help.
			context.Flush();
		}
		slot.read = Some(query);
		Ok(Surface::Texture(texture))
	}
}

impl Drop for Copy {
	fn drop(&mut self) {
		if let Some(slot) = self.slot.take() {
			self.pool.lock().unwrap().free.push(slot);
		}
	}
}

#[cfg(test)]
mod tests {
	use moq_net::Timestamp;

	use super::*;
	use crate::render::export::Route;
	use crate::render::export::tests::{center, near, texture};

	/// A Direct3D12 device, which every Windows host has, if only in software.
	async fn dx12() -> (wgpu::Device, wgpu::Queue) {
		let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
			backends: wgpu::Backends::DX12,
			..wgpu::InstanceDescriptor::new_without_display_handle()
		});
		let adapter = instance
			.request_adapter(&Default::default())
			.await
			.expect("a Direct3D12 adapter");
		crate::render::request_device(&adapter, &Default::default())
			.await
			.expect("a Direct3D12 device")
	}

	/// The export reaches the encoder as an NV12 texture, never through the CPU,
	/// and the picture survives the conversion.
	#[tokio::test]
	async fn exports_an_nv12_texture() {
		let (device, queue) = dx12().await;
		let size = Size::new(64, 48);
		let mut exporter = Exporter::open(&device, &queue, size, Route::Gpu).unwrap();

		let red = texture(&device, &queue, size, [255, 0, 0, 255]);
		let mut held: Vec<_> = (0..Exporter::DEPTH)
			.map(|_| exporter.copy(&red, Timestamp::ZERO).unwrap().expect("a free slot"))
			.collect();
		assert!(
			exporter.copy(&red, Timestamp::ZERO).unwrap().is_none(),
			"the pool is capped"
		);

		let frame = held.pop().unwrap().wait().unwrap();
		let Surface::Texture(nv12) = &frame.surface else {
			panic!("exported as {:?}", std::mem::discriminant(&frame.surface));
		};
		assert_eq!((nv12.width(), nv12.height()), (size.width, size.height));
		let actual = center(&frame);
		assert!(near(actual, [255, 0, 0, 255]), "{actual:?}");

		// The waited slot comes back once its conversion is read.
		drop(held);
		for _ in 0..Exporter::DEPTH * 2 {
			exporter
				.copy(&red, Timestamp::ZERO)
				.unwrap()
				.expect("a free slot")
				.wait()
				.unwrap();
		}
	}

	/// The Media Foundation hardware encoder takes the export in place.
	#[tokio::test]
	#[ignore = "needs a Media Foundation hardware H.264 encoder"]
	async fn media_foundation_encodes_the_export_in_place() {
		let (device, queue) = dx12().await;
		let size = Size::new(640, 480);
		let mut config = crate::encode::Config::new(size.width, size.height, crate::FrameRate::new(30, 1).unwrap());
		let (mut exporter, _) = Exporter::new(&device, &queue, &mut config).await.unwrap();
		assert_eq!(exporter.route(), Route::Gpu, "Media Foundation reads textures");
		let mut encoder = crate::encode::Encoder::new(&config).unwrap();
		assert_eq!(encoder.name(), "mediafoundation");

		let red = texture(&device, &queue, size, [255, 0, 0, 255]);
		let mut encoded = Vec::new();
		for index in 0..12u64 {
			let timestamp = Timestamp::from_micros(index * 33_333).unwrap();
			let frame = exporter
				.copy(&red, timestamp)
				.unwrap()
				.expect("a free slot")
				.wait()
				.unwrap();
			encoded.extend(encoder.encode(&frame).unwrap());
		}
		encoded.extend(encoder.finish().unwrap());
		assert!(encoded.first().is_some_and(|encoded| encoded.keyframe));
	}
}

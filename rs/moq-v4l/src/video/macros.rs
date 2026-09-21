macro_rules! impl_enum_frameintervals {
	() => {
		fn enum_frameintervals(&self, fourcc: FourCC, width: u32, height: u32) -> io::Result<Vec<FrameInterval>> {
			let mut frameintervals = Vec::new();
			let mut v4l2_struct = v4l2_frmivalenum {
				index: 0,
				pixel_format: fourcc.into(),
				width,
				height,
				..unsafe { mem::zeroed() }
			};

			loop {
				let ret = unsafe {
					v4l2::ioctl(
						self.handle().fd(),
						v4l2::vidioc::VIDIOC_ENUM_FRAMEINTERVALS,
						&mut v4l2_struct as *mut _ as *mut std::os::raw::c_void,
					)
				};

				if ret.is_err() {
					if v4l2_struct.index == 0 {
						return Err(ret.err().unwrap());
					} else {
						return Ok(frameintervals);
					}
				}

				if let Ok(frame_interval) = FrameInterval::try_from(v4l2_struct) {
					frameintervals.push(frame_interval);
				}

				v4l2_struct.index += 1;
			}
		}
	};
}

macro_rules! impl_enum_framesizes {
	() => {
		fn enum_framesizes(&self, fourcc: FourCC) -> io::Result<Vec<FrameSize>> {
			let mut framesizes = Vec::new();
			let mut v4l2_struct = v4l2_frmsizeenum {
				index: 0,
				pixel_format: fourcc.into(),
				..unsafe { mem::zeroed() }
			};

			loop {
				let ret = unsafe {
					v4l2::ioctl(
						self.handle().fd(),
						v4l2::vidioc::VIDIOC_ENUM_FRAMESIZES,
						&mut v4l2_struct as *mut _ as *mut std::os::raw::c_void,
					)
				};

				if ret.is_err() {
					if v4l2_struct.index == 0 {
						return Err(ret.err().unwrap());
					} else {
						return Ok(framesizes);
					}
				}

				if let Ok(frame_size) = FrameSize::try_from(v4l2_struct) {
					framesizes.push(frame_size);
				}

				v4l2_struct.index += 1;
			}
		}
	};
}

macro_rules! impl_enum_formats {
	($typ:expr) => {
		fn enum_formats(&self) -> io::Result<Vec<FormatDescription>> {
			let mut formats: Vec<FormatDescription> = Vec::new();
			let mut v4l2_fmt = v4l2_fmtdesc {
				index: 0,
				type_: $typ as u32,
				..unsafe { mem::zeroed() }
			};

			let mut ret: io::Result<()>;

			unsafe {
				ret = v4l2::ioctl(
					self.handle().fd(),
					v4l2::vidioc::VIDIOC_ENUM_FMT,
					&mut v4l2_fmt as *mut _ as *mut std::os::raw::c_void,
				);
			}

			if ret.is_err() {
				// Enumerating the first format (at index 0) failed, so there are no formats available
				// for this device. Just return an empty vec in this case.
				return Ok(Vec::new());
			}

			while ret.is_ok() {
				formats.push(FormatDescription::from(v4l2_fmt));
				v4l2_fmt.index += 1;

				unsafe {
					v4l2_fmt.description = mem::zeroed();
				}

				unsafe {
					ret = v4l2::ioctl(
						self.handle().fd(),
						v4l2::vidioc::VIDIOC_ENUM_FMT,
						&mut v4l2_fmt as *mut _ as *mut std::os::raw::c_void,
					);
				}
			}

			Ok(formats)
		}
	};
}

macro_rules! impl_format {
	($typ:expr) => {
		fn format(&self) -> io::Result<Format> {
			unsafe {
				let mut v4l2_fmt = v4l2_format {
					type_: $typ as u32,
					..mem::zeroed()
				};
				v4l2::ioctl(
					self.handle().fd(),
					v4l2::vidioc::VIDIOC_G_FMT,
					&mut v4l2_fmt as *mut _ as *mut std::os::raw::c_void,
				)?;

				Ok(Format::from(v4l2_fmt.fmt.pix))
			}
		}
	};
}

macro_rules! impl_set_format {
	($typ:expr) => {
		fn set_format(&self, fmt: &Format) -> io::Result<Format> {
			unsafe {
				let mut v4l2_fmt = v4l2_format {
					type_: $typ as u32,
					fmt: v4l2_format__bindgen_ty_1 { pix: (*fmt).into() },
				};
				v4l2::ioctl(
					self.handle().fd(),
					v4l2::vidioc::VIDIOC_S_FMT,
					&mut v4l2_fmt as *mut _ as *mut std::os::raw::c_void,
				)?;
			}

			self.format()
		}
	};
}

package moq

// #include <moq.h>
import "C"

import (
	"bytes"
	"encoding/binary"
	"fmt"
	"io"
	"math"
	"reflect"
	"runtime"
	"runtime/cgo"
	"sync/atomic"
	"unsafe"
)

// This is needed, because as of go 1.24
// type RustBuffer C.RustBuffer cannot have methods,
// RustBuffer is treated as non-local type
type GoRustBuffer struct {
	inner C.RustBuffer
}

type RustBufferI interface {
	AsReader() *bytes.Reader
	Free()
	ToGoBytes() []byte
	Data() unsafe.Pointer
	Len() uint64
	Capacity() uint64
}

// C.RustBuffer fields exposed as an interface so they can be accessed in different Go packages.
// See https://github.com/golang/go/issues/13467
type ExternalCRustBuffer interface {
	Data() unsafe.Pointer
	Len() uint64
	Capacity() uint64
}

func RustBufferFromC(b C.RustBuffer) ExternalCRustBuffer {
	return GoRustBuffer{
		inner: b,
	}
}

func CFromRustBuffer(b ExternalCRustBuffer) C.RustBuffer {
	return C.RustBuffer{
		capacity: C.uint64_t(b.Capacity()),
		len:      C.uint64_t(b.Len()),
		data:     (*C.uchar)(b.Data()),
	}
}

func RustBufferFromExternal(b ExternalCRustBuffer) GoRustBuffer {
	return GoRustBuffer{
		inner: C.RustBuffer{
			capacity: C.uint64_t(b.Capacity()),
			len:      C.uint64_t(b.Len()),
			data:     (*C.uchar)(b.Data()),
		},
	}
}

func (cb GoRustBuffer) Capacity() uint64 {
	return uint64(cb.inner.capacity)
}

func (cb GoRustBuffer) Len() uint64 {
	return uint64(cb.inner.len)
}

func (cb GoRustBuffer) Data() unsafe.Pointer {
	return unsafe.Pointer(cb.inner.data)
}

func (cb GoRustBuffer) AsReader() *bytes.Reader {
	b := unsafe.Slice((*byte)(cb.inner.data), C.uint64_t(cb.inner.len))
	return bytes.NewReader(b)
}

func (cb GoRustBuffer) Free() {
	rustCall(func(status *C.RustCallStatus) bool {
		C.ffi_moq_ffi_rustbuffer_free(cb.inner, status)
		return false
	})
}

func (cb GoRustBuffer) ToGoBytes() []byte {
	return C.GoBytes(unsafe.Pointer(cb.inner.data), C.int(cb.inner.len))
}

func stringToRustBuffer(str string) C.RustBuffer {
	return bytesToRustBuffer([]byte(str))
}

func bytesToRustBuffer(b []byte) C.RustBuffer {
	if len(b) == 0 {
		return C.RustBuffer{}
	}
	// We can pass the pointer along here, as it is pinned
	// for the duration of this call
	foreign := C.ForeignBytes{
		len:  C.int(len(b)),
		data: (*C.uchar)(unsafe.Pointer(&b[0])),
	}

	return rustCall(func(status *C.RustCallStatus) C.RustBuffer {
		return C.ffi_moq_ffi_rustbuffer_from_bytes(foreign, status)
	})
}

type BufLifter[GoType any] interface {
	Lift(value RustBufferI) GoType
}

type BufLowerer[GoType any] interface {
	Lower(value GoType) C.RustBuffer
}

type BufReader[GoType any] interface {
	Read(reader io.Reader) GoType
}

type BufWriter[GoType any] interface {
	Write(writer io.Writer, value GoType)
}

func LowerIntoRustBuffer[GoType any](bufWriter BufWriter[GoType], value GoType) C.RustBuffer {
	// This might be not the most efficient way but it does not require knowing allocation size
	// beforehand
	var buffer bytes.Buffer
	bufWriter.Write(&buffer, value)

	bytes, err := io.ReadAll(&buffer)
	if err != nil {
		panic(fmt.Errorf("reading written data: %w", err))
	}
	return bytesToRustBuffer(bytes)
}

func LiftFromRustBuffer[GoType any](bufReader BufReader[GoType], rbuf RustBufferI) GoType {
	defer rbuf.Free()
	reader := rbuf.AsReader()
	item := bufReader.Read(reader)
	if reader.Len() > 0 {
		// TODO: Remove this
		leftover, _ := io.ReadAll(reader)
		panic(fmt.Errorf("Junk remaining in buffer after lifting: %s", string(leftover)))
	}
	return item
}

func rustCallWithError[E any, U any](converter BufReader[E], callback func(*C.RustCallStatus) U) (U, E) {
	var status C.RustCallStatus
	returnValue := callback(&status)
	err := checkCallStatus(converter, status)
	return returnValue, err
}

func checkCallStatus[E any](converter BufReader[E], status C.RustCallStatus) E {
	switch status.code {
	case 0:
		var zero E
		return zero
	case 1:
		return LiftFromRustBuffer(converter, GoRustBuffer{inner: status.errorBuf})
	case 2:
		// when the rust code sees a panic, it tries to construct a rustBuffer
		// with the message.  but if that code panics, then it just sends back
		// an empty buffer.
		if status.errorBuf.len > 0 {
			panic(fmt.Errorf("%s", FfiConverterStringINSTANCE.Lift(GoRustBuffer{inner: status.errorBuf})))
		} else {
			panic(fmt.Errorf("Rust panicked while handling Rust panic"))
		}
	default:
		panic(fmt.Errorf("unknown status code: %d", status.code))
	}
}

func checkCallStatusUnknown(status C.RustCallStatus) error {
	switch status.code {
	case 0:
		return nil
	case 1:
		panic(fmt.Errorf("function not returning an error returned an error"))
	case 2:
		// when the rust code sees a panic, it tries to construct a C.RustBuffer
		// with the message.  but if that code panics, then it just sends back
		// an empty buffer.
		if status.errorBuf.len > 0 {
			panic(fmt.Errorf("%s", FfiConverterStringINSTANCE.Lift(GoRustBuffer{
				inner: status.errorBuf,
			})))
		} else {
			panic(fmt.Errorf("Rust panicked while handling Rust panic"))
		}
	default:
		return fmt.Errorf("unknown status code: %d", status.code)
	}
}

func rustCall[U any](callback func(*C.RustCallStatus) U) U {
	returnValue, err := rustCallWithError[error](nil, callback)
	if err != nil {
		panic(err)
	}
	return returnValue
}

type NativeError interface {
	AsError() error
}

func writeInt8(writer io.Writer, value int8) {
	if err := binary.Write(writer, binary.BigEndian, value); err != nil {
		panic(err)
	}
}

func writeUint8(writer io.Writer, value uint8) {
	if err := binary.Write(writer, binary.BigEndian, value); err != nil {
		panic(err)
	}
}

func writeInt16(writer io.Writer, value int16) {
	if err := binary.Write(writer, binary.BigEndian, value); err != nil {
		panic(err)
	}
}

func writeUint16(writer io.Writer, value uint16) {
	if err := binary.Write(writer, binary.BigEndian, value); err != nil {
		panic(err)
	}
}

func writeInt32(writer io.Writer, value int32) {
	if err := binary.Write(writer, binary.BigEndian, value); err != nil {
		panic(err)
	}
}

func writeUint32(writer io.Writer, value uint32) {
	if err := binary.Write(writer, binary.BigEndian, value); err != nil {
		panic(err)
	}
}

func writeInt64(writer io.Writer, value int64) {
	if err := binary.Write(writer, binary.BigEndian, value); err != nil {
		panic(err)
	}
}

func writeUint64(writer io.Writer, value uint64) {
	if err := binary.Write(writer, binary.BigEndian, value); err != nil {
		panic(err)
	}
}

func writeFloat32(writer io.Writer, value float32) {
	if err := binary.Write(writer, binary.BigEndian, value); err != nil {
		panic(err)
	}
}

func writeFloat64(writer io.Writer, value float64) {
	if err := binary.Write(writer, binary.BigEndian, value); err != nil {
		panic(err)
	}
}

func readInt8(reader io.Reader) int8 {
	var result int8
	if err := binary.Read(reader, binary.BigEndian, &result); err != nil {
		panic(err)
	}
	return result
}

func readUint8(reader io.Reader) uint8 {
	var result uint8
	if err := binary.Read(reader, binary.BigEndian, &result); err != nil {
		panic(err)
	}
	return result
}

func readInt16(reader io.Reader) int16 {
	var result int16
	if err := binary.Read(reader, binary.BigEndian, &result); err != nil {
		panic(err)
	}
	return result
}

func readUint16(reader io.Reader) uint16 {
	var result uint16
	if err := binary.Read(reader, binary.BigEndian, &result); err != nil {
		panic(err)
	}
	return result
}

func readInt32(reader io.Reader) int32 {
	var result int32
	if err := binary.Read(reader, binary.BigEndian, &result); err != nil {
		panic(err)
	}
	return result
}

func readUint32(reader io.Reader) uint32 {
	var result uint32
	if err := binary.Read(reader, binary.BigEndian, &result); err != nil {
		panic(err)
	}
	return result
}

func readInt64(reader io.Reader) int64 {
	var result int64
	if err := binary.Read(reader, binary.BigEndian, &result); err != nil {
		panic(err)
	}
	return result
}

func readUint64(reader io.Reader) uint64 {
	var result uint64
	if err := binary.Read(reader, binary.BigEndian, &result); err != nil {
		panic(err)
	}
	return result
}

func readFloat32(reader io.Reader) float32 {
	var result float32
	if err := binary.Read(reader, binary.BigEndian, &result); err != nil {
		panic(err)
	}
	return result
}

func readFloat64(reader io.Reader) float64 {
	var result float64
	if err := binary.Read(reader, binary.BigEndian, &result); err != nil {
		panic(err)
	}
	return result
}

func init() {

	uniffiCheckChecksums()
}

func uniffiCheckChecksums() {
	// Get the bindings contract version from our ComponentInterface
	bindingsContractVersion := 30
	// Get the scaffolding contract version by calling the into the dylib
	scaffoldingContractVersion := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint32_t {
		return C.ffi_moq_ffi_uniffi_contract_version()
	})
	if bindingsContractVersion != int(scaffoldingContractVersion) {
		// If this happens try cleaning and rebuilding your project
		panic("moq: UniFFI contract version mismatch")
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_func_moq_log_level()
		})
		if checksum != 27140 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_func_moq_log_level: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_method_moqbroadcastconsumer_subscribe_catalog()
		})
		if checksum != 28366 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_method_moqbroadcastconsumer_subscribe_catalog: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_method_moqbroadcastconsumer_subscribe_media()
		})
		if checksum != 62819 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_method_moqbroadcastconsumer_subscribe_media: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_method_moqbroadcastconsumer_subscribe_track()
		})
		if checksum != 423 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_method_moqbroadcastconsumer_subscribe_track: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_method_moqcatalogconsumer_cancel()
		})
		if checksum != 1059 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_method_moqcatalogconsumer_cancel: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_method_moqcatalogconsumer_next()
		})
		if checksum != 42881 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_method_moqcatalogconsumer_next: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_method_moqgroupconsumer_cancel()
		})
		if checksum != 21782 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_method_moqgroupconsumer_cancel: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_method_moqgroupconsumer_read_frame()
		})
		if checksum != 28945 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_method_moqgroupconsumer_read_frame: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_method_moqgroupconsumer_sequence()
		})
		if checksum != 61070 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_method_moqgroupconsumer_sequence: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_method_moqmediaconsumer_cancel()
		})
		if checksum != 12542 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_method_moqmediaconsumer_cancel: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_method_moqmediaconsumer_next()
		})
		if checksum != 26125 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_method_moqmediaconsumer_next: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_method_moqtrackconsumer_cancel()
		})
		if checksum != 13373 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_method_moqtrackconsumer_cancel: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_method_moqtrackconsumer_next_group()
		})
		if checksum != 38789 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_method_moqtrackconsumer_next_group: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_method_moqtrackconsumer_read_frame()
		})
		if checksum != 36690 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_method_moqtrackconsumer_read_frame: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_method_moqtrackconsumer_recv_group()
		})
		if checksum != 26719 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_method_moqtrackconsumer_recv_group: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_method_moqannounced_cancel()
		})
		if checksum != 11787 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_method_moqannounced_cancel: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_method_moqannounced_next()
		})
		if checksum != 30814 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_method_moqannounced_next: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_method_moqannouncedbroadcast_available()
		})
		if checksum != 46046 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_method_moqannouncedbroadcast_available: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_method_moqannouncedbroadcast_cancel()
		})
		if checksum != 63780 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_method_moqannouncedbroadcast_cancel: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_method_moqannouncement_broadcast()
		})
		if checksum != 8318 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_method_moqannouncement_broadcast: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_method_moqannouncement_path()
		})
		if checksum != 33642 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_method_moqannouncement_path: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_method_moqoriginconsumer_announced()
		})
		if checksum != 65430 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_method_moqoriginconsumer_announced: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_method_moqoriginconsumer_announced_broadcast()
		})
		if checksum != 54838 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_method_moqoriginconsumer_announced_broadcast: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_method_moqoriginproducer_consume()
		})
		if checksum != 34292 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_method_moqoriginproducer_consume: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_method_moqoriginproducer_publish()
		})
		if checksum != 24937 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_method_moqoriginproducer_publish: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_method_moqbroadcastproducer_consume()
		})
		if checksum != 46595 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_method_moqbroadcastproducer_consume: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_method_moqbroadcastproducer_finish()
		})
		if checksum != 23327 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_method_moqbroadcastproducer_finish: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_method_moqbroadcastproducer_publish_media()
		})
		if checksum != 59397 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_method_moqbroadcastproducer_publish_media: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_method_moqbroadcastproducer_publish_track()
		})
		if checksum != 63909 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_method_moqbroadcastproducer_publish_track: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_method_moqgroupproducer_consume()
		})
		if checksum != 12315 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_method_moqgroupproducer_consume: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_method_moqgroupproducer_finish()
		})
		if checksum != 39760 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_method_moqgroupproducer_finish: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_method_moqgroupproducer_sequence()
		})
		if checksum != 11821 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_method_moqgroupproducer_sequence: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_method_moqgroupproducer_write_frame()
		})
		if checksum != 35582 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_method_moqgroupproducer_write_frame: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_method_moqmediaproducer_finish()
		})
		if checksum != 13508 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_method_moqmediaproducer_finish: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_method_moqmediaproducer_name()
		})
		if checksum != 45039 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_method_moqmediaproducer_name: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_method_moqmediaproducer_unused()
		})
		if checksum != 45236 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_method_moqmediaproducer_unused: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_method_moqmediaproducer_used()
		})
		if checksum != 49162 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_method_moqmediaproducer_used: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_method_moqmediaproducer_write_frame()
		})
		if checksum != 4813 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_method_moqmediaproducer_write_frame: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_method_moqtrackproducer_append_group()
		})
		if checksum != 28433 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_method_moqtrackproducer_append_group: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_method_moqtrackproducer_consume()
		})
		if checksum != 57360 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_method_moqtrackproducer_consume: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_method_moqtrackproducer_finish()
		})
		if checksum != 52719 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_method_moqtrackproducer_finish: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_method_moqtrackproducer_name()
		})
		if checksum != 18320 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_method_moqtrackproducer_name: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_method_moqtrackproducer_unused()
		})
		if checksum != 40969 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_method_moqtrackproducer_unused: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_method_moqtrackproducer_used()
		})
		if checksum != 20539 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_method_moqtrackproducer_used: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_method_moqtrackproducer_write_frame()
		})
		if checksum != 62709 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_method_moqtrackproducer_write_frame: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_method_moqclient_cancel()
		})
		if checksum != 42343 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_method_moqclient_cancel: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_method_moqclient_connect()
		})
		if checksum != 27457 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_method_moqclient_connect: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_method_moqclient_set_consume()
		})
		if checksum != 55200 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_method_moqclient_set_consume: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_method_moqclient_set_publish()
		})
		if checksum != 56893 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_method_moqclient_set_publish: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_method_moqclient_set_tls_disable_verify()
		})
		if checksum != 17397 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_method_moqclient_set_tls_disable_verify: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_method_moqsession_cancel()
		})
		if checksum != 24930 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_method_moqsession_cancel: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_method_moqsession_closed()
		})
		if checksum != 41657 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_method_moqsession_closed: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_constructor_moqoriginproducer_new()
		})
		if checksum != 8988 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_constructor_moqoriginproducer_new: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_constructor_moqbroadcastproducer_new()
		})
		if checksum != 4251 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_constructor_moqbroadcastproducer_new: UniFFI API checksum mismatch")
		}
	}
	{
		checksum := rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint16_t {
			return C.uniffi_moq_ffi_checksum_constructor_moqclient_new()
		})
		if checksum != 62327 {
			// If this happens try cleaning and rebuilding your project
			panic("moq: uniffi_moq_ffi_checksum_constructor_moqclient_new: UniFFI API checksum mismatch")
		}
	}
}

type FfiConverterUint32 struct{}

var FfiConverterUint32INSTANCE = FfiConverterUint32{}

func (FfiConverterUint32) Lower(value uint32) C.uint32_t {
	return C.uint32_t(value)
}

func (FfiConverterUint32) Write(writer io.Writer, value uint32) {
	writeUint32(writer, value)
}

func (FfiConverterUint32) Lift(value C.uint32_t) uint32 {
	return uint32(value)
}

func (FfiConverterUint32) Read(reader io.Reader) uint32 {
	return readUint32(reader)
}

type FfiDestroyerUint32 struct{}

func (FfiDestroyerUint32) Destroy(_ uint32) {}

type FfiConverterUint64 struct{}

var FfiConverterUint64INSTANCE = FfiConverterUint64{}

func (FfiConverterUint64) Lower(value uint64) C.uint64_t {
	return C.uint64_t(value)
}

func (FfiConverterUint64) Write(writer io.Writer, value uint64) {
	writeUint64(writer, value)
}

func (FfiConverterUint64) Lift(value C.uint64_t) uint64 {
	return uint64(value)
}

func (FfiConverterUint64) Read(reader io.Reader) uint64 {
	return readUint64(reader)
}

type FfiDestroyerUint64 struct{}

func (FfiDestroyerUint64) Destroy(_ uint64) {}

type FfiConverterFloat64 struct{}

var FfiConverterFloat64INSTANCE = FfiConverterFloat64{}

func (FfiConverterFloat64) Lower(value float64) C.double {
	return C.double(value)
}

func (FfiConverterFloat64) Write(writer io.Writer, value float64) {
	writeFloat64(writer, value)
}

func (FfiConverterFloat64) Lift(value C.double) float64 {
	return float64(value)
}

func (FfiConverterFloat64) Read(reader io.Reader) float64 {
	return readFloat64(reader)
}

type FfiDestroyerFloat64 struct{}

func (FfiDestroyerFloat64) Destroy(_ float64) {}

type FfiConverterBool struct{}

var FfiConverterBoolINSTANCE = FfiConverterBool{}

func (FfiConverterBool) Lower(value bool) C.int8_t {
	if value {
		return C.int8_t(1)
	}
	return C.int8_t(0)
}

func (FfiConverterBool) Write(writer io.Writer, value bool) {
	if value {
		writeInt8(writer, 1)
	} else {
		writeInt8(writer, 0)
	}
}

func (FfiConverterBool) Lift(value C.int8_t) bool {
	return value != 0
}

func (FfiConverterBool) Read(reader io.Reader) bool {
	return readInt8(reader) != 0
}

type FfiDestroyerBool struct{}

func (FfiDestroyerBool) Destroy(_ bool) {}

type FfiConverterString struct{}

var FfiConverterStringINSTANCE = FfiConverterString{}

func (FfiConverterString) Lift(rb RustBufferI) string {
	defer rb.Free()
	reader := rb.AsReader()
	b, err := io.ReadAll(reader)
	if err != nil {
		panic(fmt.Errorf("reading reader: %w", err))
	}
	return string(b)
}

func (FfiConverterString) Read(reader io.Reader) string {
	length := readInt32(reader)
	buffer := make([]byte, length)
	read_length, err := reader.Read(buffer)
	if err != nil && err != io.EOF {
		panic(err)
	}
	if read_length != int(length) {
		panic(fmt.Errorf("bad read length when reading string, expected %d, read %d", length, read_length))
	}
	return string(buffer)
}

func (FfiConverterString) Lower(value string) C.RustBuffer {
	return stringToRustBuffer(value)
}

func (c FfiConverterString) LowerExternal(value string) ExternalCRustBuffer {
	return RustBufferFromC(stringToRustBuffer(value))
}

func (FfiConverterString) Write(writer io.Writer, value string) {
	if len(value) > math.MaxInt32 {
		panic("String is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	write_length, err := io.WriteString(writer, value)
	if err != nil {
		panic(err)
	}
	if write_length != len(value) {
		panic(fmt.Errorf("bad write length when writing string, expected %d, written %d", len(value), write_length))
	}
}

type FfiDestroyerString struct{}

func (FfiDestroyerString) Destroy(_ string) {}

type FfiConverterBytes struct{}

var FfiConverterBytesINSTANCE = FfiConverterBytes{}

func (c FfiConverterBytes) Lower(value []byte) C.RustBuffer {
	return LowerIntoRustBuffer[[]byte](c, value)
}

func (c FfiConverterBytes) LowerExternal(value []byte) ExternalCRustBuffer {
	return RustBufferFromC(c.Lower(value))
}

func (c FfiConverterBytes) Write(writer io.Writer, value []byte) {
	if len(value) > math.MaxInt32 {
		panic("[]byte is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(value)))
	write_length, err := writer.Write(value)
	if err != nil {
		panic(err)
	}
	if write_length != len(value) {
		panic(fmt.Errorf("bad write length when writing []byte, expected %d, written %d", len(value), write_length))
	}
}

func (c FfiConverterBytes) Lift(rb RustBufferI) []byte {
	return LiftFromRustBuffer[[]byte](c, rb)
}

func (c FfiConverterBytes) Read(reader io.Reader) []byte {
	length := readInt32(reader)
	buffer := make([]byte, length)
	read_length, err := reader.Read(buffer)
	if err != nil && err != io.EOF {
		panic(err)
	}
	if read_length != int(length) {
		panic(fmt.Errorf("bad read length when reading []byte, expected %d, read %d", length, read_length))
	}
	return buffer
}

type FfiDestroyerBytes struct{}

func (FfiDestroyerBytes) Destroy(_ []byte) {}

// Below is an implementation of synchronization requirements outlined in the link.
// https://github.com/mozilla/uniffi-rs/blob/0dc031132d9493ca812c3af6e7dd60ad2ea95bf0/uniffi_bindgen/src/bindings/kotlin/templates/ObjectRuntime.kt#L31

type FfiObject struct {
	handle        C.uint64_t
	callCounter   atomic.Int64
	cloneFunction func(C.uint64_t, *C.RustCallStatus) C.uint64_t
	freeFunction  func(C.uint64_t, *C.RustCallStatus)
	destroyed     atomic.Bool
}

func newFfiObject(
	handle C.uint64_t,
	cloneFunction func(C.uint64_t, *C.RustCallStatus) C.uint64_t,
	freeFunction func(C.uint64_t, *C.RustCallStatus),
) FfiObject {
	return FfiObject{
		handle:        handle,
		cloneFunction: cloneFunction,
		freeFunction:  freeFunction,
	}
}

func (ffiObject *FfiObject) incrementPointer(debugName string) C.uint64_t {
	for {
		counter := ffiObject.callCounter.Load()
		if counter <= -1 {
			panic(fmt.Errorf("%v object has already been destroyed", debugName))
		}
		if counter == math.MaxInt64 {
			panic(fmt.Errorf("%v object call counter would overflow", debugName))
		}
		if ffiObject.callCounter.CompareAndSwap(counter, counter+1) {
			break
		}
	}

	return rustCall(func(status *C.RustCallStatus) C.uint64_t {
		return ffiObject.cloneFunction(ffiObject.handle, status)
	})
}

func (ffiObject *FfiObject) decrementPointer() {
	if ffiObject.callCounter.Add(-1) == -1 {
		ffiObject.freeRustArcPtr()
	}
}

func (ffiObject *FfiObject) destroy() {
	if ffiObject.destroyed.CompareAndSwap(false, true) {
		if ffiObject.callCounter.Add(-1) == -1 {
			ffiObject.freeRustArcPtr()
		}
	}
}

func (ffiObject *FfiObject) freeRustArcPtr() {
	if ffiObject.handle == 0 {
		return
	}
	rustCall(func(status *C.RustCallStatus) int32 {
		ffiObject.freeFunction(ffiObject.handle, status)
		return 0
	})
}

type MoqAnnouncedInterface interface {
	// Cancel all current and future `next()` calls.
	Cancel()
	// Get the next broadcast announcement. Returns `None` when the origin is closed.
	//
	// Use `broadcast.closed()` to learn when a broadcast is unannounced.
	Next() (**MoqAnnouncement, error)
}
type MoqAnnounced struct {
	ffiObject FfiObject
}

// Cancel all current and future `next()` calls.
func (_self *MoqAnnounced) Cancel() {
	_pointer := _self.ffiObject.incrementPointer("*MoqAnnounced")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_moq_ffi_fn_method_moqannounced_cancel(
			_pointer, _uniffiStatus)
		return false
	})
}

// Get the next broadcast announcement. Returns `None` when the origin is closed.
//
// Use `broadcast.closed()` to learn when a broadcast is unannounced.
func (_self *MoqAnnounced) Next() (**MoqAnnouncement, error) {
	_pointer := _self.ffiObject.incrementPointer("*MoqAnnounced")
	defer _self.ffiObject.decrementPointer()
	res, err := uniffiRustCallAsync[*MoqError](
		FfiConverterMoqErrorINSTANCE,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) RustBufferI {
			res := C.ffi_moq_ffi_rust_future_complete_rust_buffer(handle, status)
			return GoRustBuffer{
				inner: res,
			}
		},
		// liftFn
		func(ffi RustBufferI) **MoqAnnouncement {
			return FfiConverterOptionalMoqAnnouncementINSTANCE.Lift(ffi)
		},
		C.uniffi_moq_ffi_fn_method_moqannounced_next(
			_pointer),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_moq_ffi_rust_future_poll_rust_buffer(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_moq_ffi_rust_future_free_rust_buffer(handle)
		},
	)

	if err == nil {
		return res, nil
	}

	return res, err
}
func (object *MoqAnnounced) Destroy() {
	runtime.SetFinalizer(object, nil)
	object.ffiObject.destroy()
}

type FfiConverterMoqAnnounced struct{}

var FfiConverterMoqAnnouncedINSTANCE = FfiConverterMoqAnnounced{}

func (c FfiConverterMoqAnnounced) Lift(handle C.uint64_t) *MoqAnnounced {
	result := &MoqAnnounced{
		newFfiObject(
			handle,
			func(handle C.uint64_t, status *C.RustCallStatus) C.uint64_t {
				return C.uniffi_moq_ffi_fn_clone_moqannounced(handle, status)
			},
			func(handle C.uint64_t, status *C.RustCallStatus) {
				C.uniffi_moq_ffi_fn_free_moqannounced(handle, status)
			},
		),
	}
	runtime.SetFinalizer(result, (*MoqAnnounced).Destroy)
	return result
}

func (c FfiConverterMoqAnnounced) Read(reader io.Reader) *MoqAnnounced {
	return c.Lift(C.uint64_t(readUint64(reader)))
}

func (c FfiConverterMoqAnnounced) Lower(value *MoqAnnounced) C.uint64_t {
	// TODO: this is bad - all synchronization from ObjectRuntime.go is discarded here,
	// because the handle will be decremented immediately after this function returns,
	// and someone will be left holding onto a non-locked handle.
	handle := value.ffiObject.incrementPointer("*MoqAnnounced")
	defer value.ffiObject.decrementPointer()
	return handle
}

func (c FfiConverterMoqAnnounced) Write(writer io.Writer, value *MoqAnnounced) {
	writeUint64(writer, uint64(c.Lower(value)))
}

func LiftFromExternalMoqAnnounced(handle uint64) *MoqAnnounced {
	return FfiConverterMoqAnnouncedINSTANCE.Lift(C.uint64_t(handle))
}

func LowerToExternalMoqAnnounced(value *MoqAnnounced) uint64 {
	return uint64(FfiConverterMoqAnnouncedINSTANCE.Lower(value))
}

type FfiDestroyerMoqAnnounced struct{}

func (_ FfiDestroyerMoqAnnounced) Destroy(value *MoqAnnounced) {
	value.Destroy()
}

// Waits for a specific broadcast to be announced.
type MoqAnnouncedBroadcastInterface interface {
	// Wait until the broadcast is announced. Returns `Closed` if cancelled or the origin is closed.
	//
	// Use `broadcast.closed()` to learn when a broadcast is unannounced.
	Available() (*MoqBroadcastConsumer, error)
	// Cancel all current and future `available()` calls.
	Cancel()
}

// Waits for a specific broadcast to be announced.
type MoqAnnouncedBroadcast struct {
	ffiObject FfiObject
}

// Wait until the broadcast is announced. Returns `Closed` if cancelled or the origin is closed.
//
// Use `broadcast.closed()` to learn when a broadcast is unannounced.
func (_self *MoqAnnouncedBroadcast) Available() (*MoqBroadcastConsumer, error) {
	_pointer := _self.ffiObject.incrementPointer("*MoqAnnouncedBroadcast")
	defer _self.ffiObject.decrementPointer()
	res, err := uniffiRustCallAsync[*MoqError](
		FfiConverterMoqErrorINSTANCE,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) C.uint64_t {
			res := C.ffi_moq_ffi_rust_future_complete_u64(handle, status)
			return res
		},
		// liftFn
		func(ffi C.uint64_t) *MoqBroadcastConsumer {
			return FfiConverterMoqBroadcastConsumerINSTANCE.Lift(ffi)
		},
		C.uniffi_moq_ffi_fn_method_moqannouncedbroadcast_available(
			_pointer),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_moq_ffi_rust_future_poll_u64(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_moq_ffi_rust_future_free_u64(handle)
		},
	)

	if err == nil {
		return res, nil
	}

	return res, err
}

// Cancel all current and future `available()` calls.
func (_self *MoqAnnouncedBroadcast) Cancel() {
	_pointer := _self.ffiObject.incrementPointer("*MoqAnnouncedBroadcast")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_moq_ffi_fn_method_moqannouncedbroadcast_cancel(
			_pointer, _uniffiStatus)
		return false
	})
}
func (object *MoqAnnouncedBroadcast) Destroy() {
	runtime.SetFinalizer(object, nil)
	object.ffiObject.destroy()
}

type FfiConverterMoqAnnouncedBroadcast struct{}

var FfiConverterMoqAnnouncedBroadcastINSTANCE = FfiConverterMoqAnnouncedBroadcast{}

func (c FfiConverterMoqAnnouncedBroadcast) Lift(handle C.uint64_t) *MoqAnnouncedBroadcast {
	result := &MoqAnnouncedBroadcast{
		newFfiObject(
			handle,
			func(handle C.uint64_t, status *C.RustCallStatus) C.uint64_t {
				return C.uniffi_moq_ffi_fn_clone_moqannouncedbroadcast(handle, status)
			},
			func(handle C.uint64_t, status *C.RustCallStatus) {
				C.uniffi_moq_ffi_fn_free_moqannouncedbroadcast(handle, status)
			},
		),
	}
	runtime.SetFinalizer(result, (*MoqAnnouncedBroadcast).Destroy)
	return result
}

func (c FfiConverterMoqAnnouncedBroadcast) Read(reader io.Reader) *MoqAnnouncedBroadcast {
	return c.Lift(C.uint64_t(readUint64(reader)))
}

func (c FfiConverterMoqAnnouncedBroadcast) Lower(value *MoqAnnouncedBroadcast) C.uint64_t {
	// TODO: this is bad - all synchronization from ObjectRuntime.go is discarded here,
	// because the handle will be decremented immediately after this function returns,
	// and someone will be left holding onto a non-locked handle.
	handle := value.ffiObject.incrementPointer("*MoqAnnouncedBroadcast")
	defer value.ffiObject.decrementPointer()
	return handle
}

func (c FfiConverterMoqAnnouncedBroadcast) Write(writer io.Writer, value *MoqAnnouncedBroadcast) {
	writeUint64(writer, uint64(c.Lower(value)))
}

func LiftFromExternalMoqAnnouncedBroadcast(handle uint64) *MoqAnnouncedBroadcast {
	return FfiConverterMoqAnnouncedBroadcastINSTANCE.Lift(C.uint64_t(handle))
}

func LowerToExternalMoqAnnouncedBroadcast(value *MoqAnnouncedBroadcast) uint64 {
	return uint64(FfiConverterMoqAnnouncedBroadcastINSTANCE.Lower(value))
}

type FfiDestroyerMoqAnnouncedBroadcast struct{}

func (_ FfiDestroyerMoqAnnouncedBroadcast) Destroy(value *MoqAnnouncedBroadcast) {
	value.Destroy()
}

// A broadcast announcement from an origin.
type MoqAnnouncementInterface interface {
	// The broadcast consumer.
	Broadcast() *MoqBroadcastConsumer
	// The path of the announced broadcast.
	Path() string
}

// A broadcast announcement from an origin.
type MoqAnnouncement struct {
	ffiObject FfiObject
}

// The broadcast consumer.
func (_self *MoqAnnouncement) Broadcast() *MoqBroadcastConsumer {
	_pointer := _self.ffiObject.incrementPointer("*MoqAnnouncement")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterMoqBroadcastConsumerINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint64_t {
		return C.uniffi_moq_ffi_fn_method_moqannouncement_broadcast(
			_pointer, _uniffiStatus)
	}))
}

// The path of the announced broadcast.
func (_self *MoqAnnouncement) Path() string {
	_pointer := _self.ffiObject.incrementPointer("*MoqAnnouncement")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterStringINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_moq_ffi_fn_method_moqannouncement_path(
				_pointer, _uniffiStatus),
		}
	}))
}
func (object *MoqAnnouncement) Destroy() {
	runtime.SetFinalizer(object, nil)
	object.ffiObject.destroy()
}

type FfiConverterMoqAnnouncement struct{}

var FfiConverterMoqAnnouncementINSTANCE = FfiConverterMoqAnnouncement{}

func (c FfiConverterMoqAnnouncement) Lift(handle C.uint64_t) *MoqAnnouncement {
	result := &MoqAnnouncement{
		newFfiObject(
			handle,
			func(handle C.uint64_t, status *C.RustCallStatus) C.uint64_t {
				return C.uniffi_moq_ffi_fn_clone_moqannouncement(handle, status)
			},
			func(handle C.uint64_t, status *C.RustCallStatus) {
				C.uniffi_moq_ffi_fn_free_moqannouncement(handle, status)
			},
		),
	}
	runtime.SetFinalizer(result, (*MoqAnnouncement).Destroy)
	return result
}

func (c FfiConverterMoqAnnouncement) Read(reader io.Reader) *MoqAnnouncement {
	return c.Lift(C.uint64_t(readUint64(reader)))
}

func (c FfiConverterMoqAnnouncement) Lower(value *MoqAnnouncement) C.uint64_t {
	// TODO: this is bad - all synchronization from ObjectRuntime.go is discarded here,
	// because the handle will be decremented immediately after this function returns,
	// and someone will be left holding onto a non-locked handle.
	handle := value.ffiObject.incrementPointer("*MoqAnnouncement")
	defer value.ffiObject.decrementPointer()
	return handle
}

func (c FfiConverterMoqAnnouncement) Write(writer io.Writer, value *MoqAnnouncement) {
	writeUint64(writer, uint64(c.Lower(value)))
}

func LiftFromExternalMoqAnnouncement(handle uint64) *MoqAnnouncement {
	return FfiConverterMoqAnnouncementINSTANCE.Lift(C.uint64_t(handle))
}

func LowerToExternalMoqAnnouncement(value *MoqAnnouncement) uint64 {
	return uint64(FfiConverterMoqAnnouncementINSTANCE.Lower(value))
}

type FfiDestroyerMoqAnnouncement struct{}

func (_ FfiDestroyerMoqAnnouncement) Destroy(value *MoqAnnouncement) {
	value.Destroy()
}

type MoqBroadcastConsumerInterface interface {
	// Subscribe to the catalog for this broadcast.
	SubscribeCatalog() (*MoqCatalogConsumer, error)
	// Subscribe to a track by name, delivering frames in decode order.
	//
	// `container` is the track container from the catalog.
	// `max_latency_ms` controls the maximum buffering before skipping a GoP.
	SubscribeMedia(name string, container Container, maxLatencyMs uint64) (*MoqMediaConsumer, error)
	// Subscribe to a track by name — same pattern as moq-boy's command/status tracks.
	//
	// Frames are returned as plain byte payloads with no codec or container parsing.
	SubscribeTrack(name string) (*MoqTrackConsumer, error)
}
type MoqBroadcastConsumer struct {
	ffiObject FfiObject
}

// Subscribe to the catalog for this broadcast.
func (_self *MoqBroadcastConsumer) SubscribeCatalog() (*MoqCatalogConsumer, error) {
	_pointer := _self.ffiObject.incrementPointer("*MoqBroadcastConsumer")
	defer _self.ffiObject.decrementPointer()
	_uniffiRV, _uniffiErr := rustCallWithError[*MoqError](FfiConverterMoqError{}, func(_uniffiStatus *C.RustCallStatus) C.uint64_t {
		return C.uniffi_moq_ffi_fn_method_moqbroadcastconsumer_subscribe_catalog(
			_pointer, _uniffiStatus)
	})
	if _uniffiErr != nil {
		var _uniffiDefaultValue *MoqCatalogConsumer
		return _uniffiDefaultValue, _uniffiErr
	} else {
		return FfiConverterMoqCatalogConsumerINSTANCE.Lift(_uniffiRV), nil
	}
}

// Subscribe to a track by name, delivering frames in decode order.
//
// `container` is the track container from the catalog.
// `max_latency_ms` controls the maximum buffering before skipping a GoP.
func (_self *MoqBroadcastConsumer) SubscribeMedia(name string, container Container, maxLatencyMs uint64) (*MoqMediaConsumer, error) {
	_pointer := _self.ffiObject.incrementPointer("*MoqBroadcastConsumer")
	defer _self.ffiObject.decrementPointer()
	_uniffiRV, _uniffiErr := rustCallWithError[*MoqError](FfiConverterMoqError{}, func(_uniffiStatus *C.RustCallStatus) C.uint64_t {
		return C.uniffi_moq_ffi_fn_method_moqbroadcastconsumer_subscribe_media(
			_pointer, FfiConverterStringINSTANCE.Lower(name), FfiConverterContainerINSTANCE.Lower(container), FfiConverterUint64INSTANCE.Lower(maxLatencyMs), _uniffiStatus)
	})
	if _uniffiErr != nil {
		var _uniffiDefaultValue *MoqMediaConsumer
		return _uniffiDefaultValue, _uniffiErr
	} else {
		return FfiConverterMoqMediaConsumerINSTANCE.Lift(_uniffiRV), nil
	}
}

// Subscribe to a track by name — same pattern as moq-boy's command/status tracks.
//
// Frames are returned as plain byte payloads with no codec or container parsing.
func (_self *MoqBroadcastConsumer) SubscribeTrack(name string) (*MoqTrackConsumer, error) {
	_pointer := _self.ffiObject.incrementPointer("*MoqBroadcastConsumer")
	defer _self.ffiObject.decrementPointer()
	_uniffiRV, _uniffiErr := rustCallWithError[*MoqError](FfiConverterMoqError{}, func(_uniffiStatus *C.RustCallStatus) C.uint64_t {
		return C.uniffi_moq_ffi_fn_method_moqbroadcastconsumer_subscribe_track(
			_pointer, FfiConverterStringINSTANCE.Lower(name), _uniffiStatus)
	})
	if _uniffiErr != nil {
		var _uniffiDefaultValue *MoqTrackConsumer
		return _uniffiDefaultValue, _uniffiErr
	} else {
		return FfiConverterMoqTrackConsumerINSTANCE.Lift(_uniffiRV), nil
	}
}
func (object *MoqBroadcastConsumer) Destroy() {
	runtime.SetFinalizer(object, nil)
	object.ffiObject.destroy()
}

type FfiConverterMoqBroadcastConsumer struct{}

var FfiConverterMoqBroadcastConsumerINSTANCE = FfiConverterMoqBroadcastConsumer{}

func (c FfiConverterMoqBroadcastConsumer) Lift(handle C.uint64_t) *MoqBroadcastConsumer {
	result := &MoqBroadcastConsumer{
		newFfiObject(
			handle,
			func(handle C.uint64_t, status *C.RustCallStatus) C.uint64_t {
				return C.uniffi_moq_ffi_fn_clone_moqbroadcastconsumer(handle, status)
			},
			func(handle C.uint64_t, status *C.RustCallStatus) {
				C.uniffi_moq_ffi_fn_free_moqbroadcastconsumer(handle, status)
			},
		),
	}
	runtime.SetFinalizer(result, (*MoqBroadcastConsumer).Destroy)
	return result
}

func (c FfiConverterMoqBroadcastConsumer) Read(reader io.Reader) *MoqBroadcastConsumer {
	return c.Lift(C.uint64_t(readUint64(reader)))
}

func (c FfiConverterMoqBroadcastConsumer) Lower(value *MoqBroadcastConsumer) C.uint64_t {
	// TODO: this is bad - all synchronization from ObjectRuntime.go is discarded here,
	// because the handle will be decremented immediately after this function returns,
	// and someone will be left holding onto a non-locked handle.
	handle := value.ffiObject.incrementPointer("*MoqBroadcastConsumer")
	defer value.ffiObject.decrementPointer()
	return handle
}

func (c FfiConverterMoqBroadcastConsumer) Write(writer io.Writer, value *MoqBroadcastConsumer) {
	writeUint64(writer, uint64(c.Lower(value)))
}

func LiftFromExternalMoqBroadcastConsumer(handle uint64) *MoqBroadcastConsumer {
	return FfiConverterMoqBroadcastConsumerINSTANCE.Lift(C.uint64_t(handle))
}

func LowerToExternalMoqBroadcastConsumer(value *MoqBroadcastConsumer) uint64 {
	return uint64(FfiConverterMoqBroadcastConsumerINSTANCE.Lower(value))
}

type FfiDestroyerMoqBroadcastConsumer struct{}

func (_ FfiDestroyerMoqBroadcastConsumer) Destroy(value *MoqBroadcastConsumer) {
	value.Destroy()
}

type MoqBroadcastProducerInterface interface {
	// Create a consumer that reads from this broadcast's tracks.
	Consume() (*MoqBroadcastConsumer, error)
	// Finish this publisher, finalizing the catalog stream.
	Finish() error
	// Create a new media track for this broadcast.
	//
	// `format` controls the encoding of `init` and frame payloads.
	PublishMedia(format string, init []byte) (*MoqMediaProducer, error)
	// Create a track for arbitrary byte payloads — no codec or container.
	//
	// Same pattern as moq-boy's `status` and `command` tracks: raw UTF-8/JSON
	// bytes written directly to moq-lite groups with no media framing.
	PublishTrack(name string) (*MoqTrackProducer, error)
}
type MoqBroadcastProducer struct {
	ffiObject FfiObject
}

// Create a new broadcast for publishing media tracks.
//
// NOTE: This will do nothing until published to an origin.
func NewMoqBroadcastProducer() (*MoqBroadcastProducer, error) {
	_uniffiRV, _uniffiErr := rustCallWithError[*MoqError](FfiConverterMoqError{}, func(_uniffiStatus *C.RustCallStatus) C.uint64_t {
		return C.uniffi_moq_ffi_fn_constructor_moqbroadcastproducer_new(_uniffiStatus)
	})
	if _uniffiErr != nil {
		var _uniffiDefaultValue *MoqBroadcastProducer
		return _uniffiDefaultValue, _uniffiErr
	} else {
		return FfiConverterMoqBroadcastProducerINSTANCE.Lift(_uniffiRV), nil
	}
}

// Create a consumer that reads from this broadcast's tracks.
func (_self *MoqBroadcastProducer) Consume() (*MoqBroadcastConsumer, error) {
	_pointer := _self.ffiObject.incrementPointer("*MoqBroadcastProducer")
	defer _self.ffiObject.decrementPointer()
	_uniffiRV, _uniffiErr := rustCallWithError[*MoqError](FfiConverterMoqError{}, func(_uniffiStatus *C.RustCallStatus) C.uint64_t {
		return C.uniffi_moq_ffi_fn_method_moqbroadcastproducer_consume(
			_pointer, _uniffiStatus)
	})
	if _uniffiErr != nil {
		var _uniffiDefaultValue *MoqBroadcastConsumer
		return _uniffiDefaultValue, _uniffiErr
	} else {
		return FfiConverterMoqBroadcastConsumerINSTANCE.Lift(_uniffiRV), nil
	}
}

// Finish this publisher, finalizing the catalog stream.
func (_self *MoqBroadcastProducer) Finish() error {
	_pointer := _self.ffiObject.incrementPointer("*MoqBroadcastProducer")
	defer _self.ffiObject.decrementPointer()
	_, _uniffiErr := rustCallWithError[*MoqError](FfiConverterMoqError{}, func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_moq_ffi_fn_method_moqbroadcastproducer_finish(
			_pointer, _uniffiStatus)
		return false
	})
	return _uniffiErr.AsError()
}

// Create a new media track for this broadcast.
//
// `format` controls the encoding of `init` and frame payloads.
func (_self *MoqBroadcastProducer) PublishMedia(format string, init []byte) (*MoqMediaProducer, error) {
	_pointer := _self.ffiObject.incrementPointer("*MoqBroadcastProducer")
	defer _self.ffiObject.decrementPointer()
	_uniffiRV, _uniffiErr := rustCallWithError[*MoqError](FfiConverterMoqError{}, func(_uniffiStatus *C.RustCallStatus) C.uint64_t {
		return C.uniffi_moq_ffi_fn_method_moqbroadcastproducer_publish_media(
			_pointer, FfiConverterStringINSTANCE.Lower(format), FfiConverterBytesINSTANCE.Lower(init), _uniffiStatus)
	})
	if _uniffiErr != nil {
		var _uniffiDefaultValue *MoqMediaProducer
		return _uniffiDefaultValue, _uniffiErr
	} else {
		return FfiConverterMoqMediaProducerINSTANCE.Lift(_uniffiRV), nil
	}
}

// Create a track for arbitrary byte payloads — no codec or container.
//
// Same pattern as moq-boy's `status` and `command` tracks: raw UTF-8/JSON
// bytes written directly to moq-lite groups with no media framing.
func (_self *MoqBroadcastProducer) PublishTrack(name string) (*MoqTrackProducer, error) {
	_pointer := _self.ffiObject.incrementPointer("*MoqBroadcastProducer")
	defer _self.ffiObject.decrementPointer()
	_uniffiRV, _uniffiErr := rustCallWithError[*MoqError](FfiConverterMoqError{}, func(_uniffiStatus *C.RustCallStatus) C.uint64_t {
		return C.uniffi_moq_ffi_fn_method_moqbroadcastproducer_publish_track(
			_pointer, FfiConverterStringINSTANCE.Lower(name), _uniffiStatus)
	})
	if _uniffiErr != nil {
		var _uniffiDefaultValue *MoqTrackProducer
		return _uniffiDefaultValue, _uniffiErr
	} else {
		return FfiConverterMoqTrackProducerINSTANCE.Lift(_uniffiRV), nil
	}
}
func (object *MoqBroadcastProducer) Destroy() {
	runtime.SetFinalizer(object, nil)
	object.ffiObject.destroy()
}

type FfiConverterMoqBroadcastProducer struct{}

var FfiConverterMoqBroadcastProducerINSTANCE = FfiConverterMoqBroadcastProducer{}

func (c FfiConverterMoqBroadcastProducer) Lift(handle C.uint64_t) *MoqBroadcastProducer {
	result := &MoqBroadcastProducer{
		newFfiObject(
			handle,
			func(handle C.uint64_t, status *C.RustCallStatus) C.uint64_t {
				return C.uniffi_moq_ffi_fn_clone_moqbroadcastproducer(handle, status)
			},
			func(handle C.uint64_t, status *C.RustCallStatus) {
				C.uniffi_moq_ffi_fn_free_moqbroadcastproducer(handle, status)
			},
		),
	}
	runtime.SetFinalizer(result, (*MoqBroadcastProducer).Destroy)
	return result
}

func (c FfiConverterMoqBroadcastProducer) Read(reader io.Reader) *MoqBroadcastProducer {
	return c.Lift(C.uint64_t(readUint64(reader)))
}

func (c FfiConverterMoqBroadcastProducer) Lower(value *MoqBroadcastProducer) C.uint64_t {
	// TODO: this is bad - all synchronization from ObjectRuntime.go is discarded here,
	// because the handle will be decremented immediately after this function returns,
	// and someone will be left holding onto a non-locked handle.
	handle := value.ffiObject.incrementPointer("*MoqBroadcastProducer")
	defer value.ffiObject.decrementPointer()
	return handle
}

func (c FfiConverterMoqBroadcastProducer) Write(writer io.Writer, value *MoqBroadcastProducer) {
	writeUint64(writer, uint64(c.Lower(value)))
}

func LiftFromExternalMoqBroadcastProducer(handle uint64) *MoqBroadcastProducer {
	return FfiConverterMoqBroadcastProducerINSTANCE.Lift(C.uint64_t(handle))
}

func LowerToExternalMoqBroadcastProducer(value *MoqBroadcastProducer) uint64 {
	return uint64(FfiConverterMoqBroadcastProducerINSTANCE.Lower(value))
}

type FfiDestroyerMoqBroadcastProducer struct{}

func (_ FfiDestroyerMoqBroadcastProducer) Destroy(value *MoqBroadcastProducer) {
	value.Destroy()
}

type MoqCatalogConsumerInterface interface {
	// Cancel all current and future `next()` calls.
	Cancel()
	// Get the next catalog update. Returns `None` when the track ends or is closed.
	Next() (*MoqCatalog, error)
}
type MoqCatalogConsumer struct {
	ffiObject FfiObject
}

// Cancel all current and future `next()` calls.
func (_self *MoqCatalogConsumer) Cancel() {
	_pointer := _self.ffiObject.incrementPointer("*MoqCatalogConsumer")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_moq_ffi_fn_method_moqcatalogconsumer_cancel(
			_pointer, _uniffiStatus)
		return false
	})
}

// Get the next catalog update. Returns `None` when the track ends or is closed.
func (_self *MoqCatalogConsumer) Next() (*MoqCatalog, error) {
	_pointer := _self.ffiObject.incrementPointer("*MoqCatalogConsumer")
	defer _self.ffiObject.decrementPointer()
	res, err := uniffiRustCallAsync[*MoqError](
		FfiConverterMoqErrorINSTANCE,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) RustBufferI {
			res := C.ffi_moq_ffi_rust_future_complete_rust_buffer(handle, status)
			return GoRustBuffer{
				inner: res,
			}
		},
		// liftFn
		func(ffi RustBufferI) *MoqCatalog {
			return FfiConverterOptionalMoqCatalogINSTANCE.Lift(ffi)
		},
		C.uniffi_moq_ffi_fn_method_moqcatalogconsumer_next(
			_pointer),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_moq_ffi_rust_future_poll_rust_buffer(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_moq_ffi_rust_future_free_rust_buffer(handle)
		},
	)

	if err == nil {
		return res, nil
	}

	return res, err
}
func (object *MoqCatalogConsumer) Destroy() {
	runtime.SetFinalizer(object, nil)
	object.ffiObject.destroy()
}

type FfiConverterMoqCatalogConsumer struct{}

var FfiConverterMoqCatalogConsumerINSTANCE = FfiConverterMoqCatalogConsumer{}

func (c FfiConverterMoqCatalogConsumer) Lift(handle C.uint64_t) *MoqCatalogConsumer {
	result := &MoqCatalogConsumer{
		newFfiObject(
			handle,
			func(handle C.uint64_t, status *C.RustCallStatus) C.uint64_t {
				return C.uniffi_moq_ffi_fn_clone_moqcatalogconsumer(handle, status)
			},
			func(handle C.uint64_t, status *C.RustCallStatus) {
				C.uniffi_moq_ffi_fn_free_moqcatalogconsumer(handle, status)
			},
		),
	}
	runtime.SetFinalizer(result, (*MoqCatalogConsumer).Destroy)
	return result
}

func (c FfiConverterMoqCatalogConsumer) Read(reader io.Reader) *MoqCatalogConsumer {
	return c.Lift(C.uint64_t(readUint64(reader)))
}

func (c FfiConverterMoqCatalogConsumer) Lower(value *MoqCatalogConsumer) C.uint64_t {
	// TODO: this is bad - all synchronization from ObjectRuntime.go is discarded here,
	// because the handle will be decremented immediately after this function returns,
	// and someone will be left holding onto a non-locked handle.
	handle := value.ffiObject.incrementPointer("*MoqCatalogConsumer")
	defer value.ffiObject.decrementPointer()
	return handle
}

func (c FfiConverterMoqCatalogConsumer) Write(writer io.Writer, value *MoqCatalogConsumer) {
	writeUint64(writer, uint64(c.Lower(value)))
}

func LiftFromExternalMoqCatalogConsumer(handle uint64) *MoqCatalogConsumer {
	return FfiConverterMoqCatalogConsumerINSTANCE.Lift(C.uint64_t(handle))
}

func LowerToExternalMoqCatalogConsumer(value *MoqCatalogConsumer) uint64 {
	return uint64(FfiConverterMoqCatalogConsumerINSTANCE.Lower(value))
}

type FfiDestroyerMoqCatalogConsumer struct{}

func (_ FfiDestroyerMoqCatalogConsumer) Destroy(value *MoqCatalogConsumer) {
	value.Destroy()
}

type MoqClientInterface interface {
	// Cancel all current and future `connect()` calls.
	Cancel()
	// Connect to a MoQ server and wait for the session to be established.
	//
	// Can be cancelled by calling `cancel()`.
	Connect(url string) (*MoqSession, error)
	// Set the origin to consume remote broadcasts from the remote.
	SetConsume(origin **MoqOriginProducer)
	// Set the origin to publish local broadcasts to the remote.
	SetPublish(origin **MoqOriginProducer)
	// Disable TLS certificate verification (for development only).
	SetTlsDisableVerify(disable bool)
}
type MoqClient struct {
	ffiObject FfiObject
}

// Create a new MoQ client with default configuration.
func NewMoqClient() *MoqClient {
	return FfiConverterMoqClientINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint64_t {
		return C.uniffi_moq_ffi_fn_constructor_moqclient_new(_uniffiStatus)
	}))
}

// Cancel all current and future `connect()` calls.
func (_self *MoqClient) Cancel() {
	_pointer := _self.ffiObject.incrementPointer("*MoqClient")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_moq_ffi_fn_method_moqclient_cancel(
			_pointer, _uniffiStatus)
		return false
	})
}

// Connect to a MoQ server and wait for the session to be established.
//
// Can be cancelled by calling `cancel()`.
func (_self *MoqClient) Connect(url string) (*MoqSession, error) {
	_pointer := _self.ffiObject.incrementPointer("*MoqClient")
	defer _self.ffiObject.decrementPointer()
	res, err := uniffiRustCallAsync[*MoqError](
		FfiConverterMoqErrorINSTANCE,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) C.uint64_t {
			res := C.ffi_moq_ffi_rust_future_complete_u64(handle, status)
			return res
		},
		// liftFn
		func(ffi C.uint64_t) *MoqSession {
			return FfiConverterMoqSessionINSTANCE.Lift(ffi)
		},
		C.uniffi_moq_ffi_fn_method_moqclient_connect(
			_pointer, FfiConverterStringINSTANCE.Lower(url)),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_moq_ffi_rust_future_poll_u64(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_moq_ffi_rust_future_free_u64(handle)
		},
	)

	if err == nil {
		return res, nil
	}

	return res, err
}

// Set the origin to consume remote broadcasts from the remote.
func (_self *MoqClient) SetConsume(origin **MoqOriginProducer) {
	_pointer := _self.ffiObject.incrementPointer("*MoqClient")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_moq_ffi_fn_method_moqclient_set_consume(
			_pointer, FfiConverterOptionalMoqOriginProducerINSTANCE.Lower(origin), _uniffiStatus)
		return false
	})
}

// Set the origin to publish local broadcasts to the remote.
func (_self *MoqClient) SetPublish(origin **MoqOriginProducer) {
	_pointer := _self.ffiObject.incrementPointer("*MoqClient")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_moq_ffi_fn_method_moqclient_set_publish(
			_pointer, FfiConverterOptionalMoqOriginProducerINSTANCE.Lower(origin), _uniffiStatus)
		return false
	})
}

// Disable TLS certificate verification (for development only).
func (_self *MoqClient) SetTlsDisableVerify(disable bool) {
	_pointer := _self.ffiObject.incrementPointer("*MoqClient")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_moq_ffi_fn_method_moqclient_set_tls_disable_verify(
			_pointer, FfiConverterBoolINSTANCE.Lower(disable), _uniffiStatus)
		return false
	})
}
func (object *MoqClient) Destroy() {
	runtime.SetFinalizer(object, nil)
	object.ffiObject.destroy()
}

type FfiConverterMoqClient struct{}

var FfiConverterMoqClientINSTANCE = FfiConverterMoqClient{}

func (c FfiConverterMoqClient) Lift(handle C.uint64_t) *MoqClient {
	result := &MoqClient{
		newFfiObject(
			handle,
			func(handle C.uint64_t, status *C.RustCallStatus) C.uint64_t {
				return C.uniffi_moq_ffi_fn_clone_moqclient(handle, status)
			},
			func(handle C.uint64_t, status *C.RustCallStatus) {
				C.uniffi_moq_ffi_fn_free_moqclient(handle, status)
			},
		),
	}
	runtime.SetFinalizer(result, (*MoqClient).Destroy)
	return result
}

func (c FfiConverterMoqClient) Read(reader io.Reader) *MoqClient {
	return c.Lift(C.uint64_t(readUint64(reader)))
}

func (c FfiConverterMoqClient) Lower(value *MoqClient) C.uint64_t {
	// TODO: this is bad - all synchronization from ObjectRuntime.go is discarded here,
	// because the handle will be decremented immediately after this function returns,
	// and someone will be left holding onto a non-locked handle.
	handle := value.ffiObject.incrementPointer("*MoqClient")
	defer value.ffiObject.decrementPointer()
	return handle
}

func (c FfiConverterMoqClient) Write(writer io.Writer, value *MoqClient) {
	writeUint64(writer, uint64(c.Lower(value)))
}

func LiftFromExternalMoqClient(handle uint64) *MoqClient {
	return FfiConverterMoqClientINSTANCE.Lift(C.uint64_t(handle))
}

func LowerToExternalMoqClient(value *MoqClient) uint64 {
	return uint64(FfiConverterMoqClientINSTANCE.Lower(value))
}

type FfiDestroyerMoqClient struct{}

func (_ FfiDestroyerMoqClient) Destroy(value *MoqClient) {
	value.Destroy()
}

type MoqGroupConsumerInterface interface {
	Cancel()
	// Read the next frame in this group. Returns `None` when the group ends.
	ReadFrame() (*[]byte, error)
	// The sequence number of this group within the track.
	Sequence() uint64
}
type MoqGroupConsumer struct {
	ffiObject FfiObject
}

func (_self *MoqGroupConsumer) Cancel() {
	_pointer := _self.ffiObject.incrementPointer("*MoqGroupConsumer")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_moq_ffi_fn_method_moqgroupconsumer_cancel(
			_pointer, _uniffiStatus)
		return false
	})
}

// Read the next frame in this group. Returns `None` when the group ends.
func (_self *MoqGroupConsumer) ReadFrame() (*[]byte, error) {
	_pointer := _self.ffiObject.incrementPointer("*MoqGroupConsumer")
	defer _self.ffiObject.decrementPointer()
	res, err := uniffiRustCallAsync[*MoqError](
		FfiConverterMoqErrorINSTANCE,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) RustBufferI {
			res := C.ffi_moq_ffi_rust_future_complete_rust_buffer(handle, status)
			return GoRustBuffer{
				inner: res,
			}
		},
		// liftFn
		func(ffi RustBufferI) *[]byte {
			return FfiConverterOptionalBytesINSTANCE.Lift(ffi)
		},
		C.uniffi_moq_ffi_fn_method_moqgroupconsumer_read_frame(
			_pointer),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_moq_ffi_rust_future_poll_rust_buffer(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_moq_ffi_rust_future_free_rust_buffer(handle)
		},
	)

	if err == nil {
		return res, nil
	}

	return res, err
}

// The sequence number of this group within the track.
func (_self *MoqGroupConsumer) Sequence() uint64 {
	_pointer := _self.ffiObject.incrementPointer("*MoqGroupConsumer")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterUint64INSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint64_t {
		return C.uniffi_moq_ffi_fn_method_moqgroupconsumer_sequence(
			_pointer, _uniffiStatus)
	}))
}
func (object *MoqGroupConsumer) Destroy() {
	runtime.SetFinalizer(object, nil)
	object.ffiObject.destroy()
}

type FfiConverterMoqGroupConsumer struct{}

var FfiConverterMoqGroupConsumerINSTANCE = FfiConverterMoqGroupConsumer{}

func (c FfiConverterMoqGroupConsumer) Lift(handle C.uint64_t) *MoqGroupConsumer {
	result := &MoqGroupConsumer{
		newFfiObject(
			handle,
			func(handle C.uint64_t, status *C.RustCallStatus) C.uint64_t {
				return C.uniffi_moq_ffi_fn_clone_moqgroupconsumer(handle, status)
			},
			func(handle C.uint64_t, status *C.RustCallStatus) {
				C.uniffi_moq_ffi_fn_free_moqgroupconsumer(handle, status)
			},
		),
	}
	runtime.SetFinalizer(result, (*MoqGroupConsumer).Destroy)
	return result
}

func (c FfiConverterMoqGroupConsumer) Read(reader io.Reader) *MoqGroupConsumer {
	return c.Lift(C.uint64_t(readUint64(reader)))
}

func (c FfiConverterMoqGroupConsumer) Lower(value *MoqGroupConsumer) C.uint64_t {
	// TODO: this is bad - all synchronization from ObjectRuntime.go is discarded here,
	// because the handle will be decremented immediately after this function returns,
	// and someone will be left holding onto a non-locked handle.
	handle := value.ffiObject.incrementPointer("*MoqGroupConsumer")
	defer value.ffiObject.decrementPointer()
	return handle
}

func (c FfiConverterMoqGroupConsumer) Write(writer io.Writer, value *MoqGroupConsumer) {
	writeUint64(writer, uint64(c.Lower(value)))
}

func LiftFromExternalMoqGroupConsumer(handle uint64) *MoqGroupConsumer {
	return FfiConverterMoqGroupConsumerINSTANCE.Lift(C.uint64_t(handle))
}

func LowerToExternalMoqGroupConsumer(value *MoqGroupConsumer) uint64 {
	return uint64(FfiConverterMoqGroupConsumerINSTANCE.Lower(value))
}

type FfiDestroyerMoqGroupConsumer struct{}

func (_ FfiDestroyerMoqGroupConsumer) Destroy(value *MoqGroupConsumer) {
	value.Destroy()
}

type MoqGroupProducerInterface interface {
	// Create a consumer that reads frames from this group.
	Consume() (*MoqGroupConsumer, error)
	// Mark the group as complete. No more frames can be written.
	Finish() error
	// The sequence number of this group within the track.
	Sequence() uint64
	// Write a frame into this group.
	WriteFrame(payload []byte) error
}
type MoqGroupProducer struct {
	ffiObject FfiObject
}

// Create a consumer that reads frames from this group.
func (_self *MoqGroupProducer) Consume() (*MoqGroupConsumer, error) {
	_pointer := _self.ffiObject.incrementPointer("*MoqGroupProducer")
	defer _self.ffiObject.decrementPointer()
	_uniffiRV, _uniffiErr := rustCallWithError[*MoqError](FfiConverterMoqError{}, func(_uniffiStatus *C.RustCallStatus) C.uint64_t {
		return C.uniffi_moq_ffi_fn_method_moqgroupproducer_consume(
			_pointer, _uniffiStatus)
	})
	if _uniffiErr != nil {
		var _uniffiDefaultValue *MoqGroupConsumer
		return _uniffiDefaultValue, _uniffiErr
	} else {
		return FfiConverterMoqGroupConsumerINSTANCE.Lift(_uniffiRV), nil
	}
}

// Mark the group as complete. No more frames can be written.
func (_self *MoqGroupProducer) Finish() error {
	_pointer := _self.ffiObject.incrementPointer("*MoqGroupProducer")
	defer _self.ffiObject.decrementPointer()
	_, _uniffiErr := rustCallWithError[*MoqError](FfiConverterMoqError{}, func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_moq_ffi_fn_method_moqgroupproducer_finish(
			_pointer, _uniffiStatus)
		return false
	})
	return _uniffiErr.AsError()
}

// The sequence number of this group within the track.
func (_self *MoqGroupProducer) Sequence() uint64 {
	_pointer := _self.ffiObject.incrementPointer("*MoqGroupProducer")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterUint64INSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint64_t {
		return C.uniffi_moq_ffi_fn_method_moqgroupproducer_sequence(
			_pointer, _uniffiStatus)
	}))
}

// Write a frame into this group.
func (_self *MoqGroupProducer) WriteFrame(payload []byte) error {
	_pointer := _self.ffiObject.incrementPointer("*MoqGroupProducer")
	defer _self.ffiObject.decrementPointer()
	_, _uniffiErr := rustCallWithError[*MoqError](FfiConverterMoqError{}, func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_moq_ffi_fn_method_moqgroupproducer_write_frame(
			_pointer, FfiConverterBytesINSTANCE.Lower(payload), _uniffiStatus)
		return false
	})
	return _uniffiErr.AsError()
}
func (object *MoqGroupProducer) Destroy() {
	runtime.SetFinalizer(object, nil)
	object.ffiObject.destroy()
}

type FfiConverterMoqGroupProducer struct{}

var FfiConverterMoqGroupProducerINSTANCE = FfiConverterMoqGroupProducer{}

func (c FfiConverterMoqGroupProducer) Lift(handle C.uint64_t) *MoqGroupProducer {
	result := &MoqGroupProducer{
		newFfiObject(
			handle,
			func(handle C.uint64_t, status *C.RustCallStatus) C.uint64_t {
				return C.uniffi_moq_ffi_fn_clone_moqgroupproducer(handle, status)
			},
			func(handle C.uint64_t, status *C.RustCallStatus) {
				C.uniffi_moq_ffi_fn_free_moqgroupproducer(handle, status)
			},
		),
	}
	runtime.SetFinalizer(result, (*MoqGroupProducer).Destroy)
	return result
}

func (c FfiConverterMoqGroupProducer) Read(reader io.Reader) *MoqGroupProducer {
	return c.Lift(C.uint64_t(readUint64(reader)))
}

func (c FfiConverterMoqGroupProducer) Lower(value *MoqGroupProducer) C.uint64_t {
	// TODO: this is bad - all synchronization from ObjectRuntime.go is discarded here,
	// because the handle will be decremented immediately after this function returns,
	// and someone will be left holding onto a non-locked handle.
	handle := value.ffiObject.incrementPointer("*MoqGroupProducer")
	defer value.ffiObject.decrementPointer()
	return handle
}

func (c FfiConverterMoqGroupProducer) Write(writer io.Writer, value *MoqGroupProducer) {
	writeUint64(writer, uint64(c.Lower(value)))
}

func LiftFromExternalMoqGroupProducer(handle uint64) *MoqGroupProducer {
	return FfiConverterMoqGroupProducerINSTANCE.Lift(C.uint64_t(handle))
}

func LowerToExternalMoqGroupProducer(value *MoqGroupProducer) uint64 {
	return uint64(FfiConverterMoqGroupProducerINSTANCE.Lower(value))
}

type FfiDestroyerMoqGroupProducer struct{}

func (_ FfiDestroyerMoqGroupProducer) Destroy(value *MoqGroupProducer) {
	value.Destroy()
}

type MoqMediaConsumerInterface interface {
	// Cancel all current and future `next()` calls.
	Cancel()
	// Get the next frame. Returns `None` when the track ends or is closed.
	Next() (*MoqFrame, error)
}
type MoqMediaConsumer struct {
	ffiObject FfiObject
}

// Cancel all current and future `next()` calls.
func (_self *MoqMediaConsumer) Cancel() {
	_pointer := _self.ffiObject.incrementPointer("*MoqMediaConsumer")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_moq_ffi_fn_method_moqmediaconsumer_cancel(
			_pointer, _uniffiStatus)
		return false
	})
}

// Get the next frame. Returns `None` when the track ends or is closed.
func (_self *MoqMediaConsumer) Next() (*MoqFrame, error) {
	_pointer := _self.ffiObject.incrementPointer("*MoqMediaConsumer")
	defer _self.ffiObject.decrementPointer()
	res, err := uniffiRustCallAsync[*MoqError](
		FfiConverterMoqErrorINSTANCE,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) RustBufferI {
			res := C.ffi_moq_ffi_rust_future_complete_rust_buffer(handle, status)
			return GoRustBuffer{
				inner: res,
			}
		},
		// liftFn
		func(ffi RustBufferI) *MoqFrame {
			return FfiConverterOptionalMoqFrameINSTANCE.Lift(ffi)
		},
		C.uniffi_moq_ffi_fn_method_moqmediaconsumer_next(
			_pointer),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_moq_ffi_rust_future_poll_rust_buffer(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_moq_ffi_rust_future_free_rust_buffer(handle)
		},
	)

	if err == nil {
		return res, nil
	}

	return res, err
}
func (object *MoqMediaConsumer) Destroy() {
	runtime.SetFinalizer(object, nil)
	object.ffiObject.destroy()
}

type FfiConverterMoqMediaConsumer struct{}

var FfiConverterMoqMediaConsumerINSTANCE = FfiConverterMoqMediaConsumer{}

func (c FfiConverterMoqMediaConsumer) Lift(handle C.uint64_t) *MoqMediaConsumer {
	result := &MoqMediaConsumer{
		newFfiObject(
			handle,
			func(handle C.uint64_t, status *C.RustCallStatus) C.uint64_t {
				return C.uniffi_moq_ffi_fn_clone_moqmediaconsumer(handle, status)
			},
			func(handle C.uint64_t, status *C.RustCallStatus) {
				C.uniffi_moq_ffi_fn_free_moqmediaconsumer(handle, status)
			},
		),
	}
	runtime.SetFinalizer(result, (*MoqMediaConsumer).Destroy)
	return result
}

func (c FfiConverterMoqMediaConsumer) Read(reader io.Reader) *MoqMediaConsumer {
	return c.Lift(C.uint64_t(readUint64(reader)))
}

func (c FfiConverterMoqMediaConsumer) Lower(value *MoqMediaConsumer) C.uint64_t {
	// TODO: this is bad - all synchronization from ObjectRuntime.go is discarded here,
	// because the handle will be decremented immediately after this function returns,
	// and someone will be left holding onto a non-locked handle.
	handle := value.ffiObject.incrementPointer("*MoqMediaConsumer")
	defer value.ffiObject.decrementPointer()
	return handle
}

func (c FfiConverterMoqMediaConsumer) Write(writer io.Writer, value *MoqMediaConsumer) {
	writeUint64(writer, uint64(c.Lower(value)))
}

func LiftFromExternalMoqMediaConsumer(handle uint64) *MoqMediaConsumer {
	return FfiConverterMoqMediaConsumerINSTANCE.Lift(C.uint64_t(handle))
}

func LowerToExternalMoqMediaConsumer(value *MoqMediaConsumer) uint64 {
	return uint64(FfiConverterMoqMediaConsumerINSTANCE.Lower(value))
}

type FfiDestroyerMoqMediaConsumer struct{}

func (_ FfiDestroyerMoqMediaConsumer) Destroy(value *MoqMediaConsumer) {
	value.Destroy()
}

type MoqMediaProducerInterface interface {
	// Finish this media track and finalize encoding.
	Finish() error
	// Return the name of the media track.
	Name() (string, error)
	// Wait until this media track has no active consumers.
	Unused() error
	// Wait until this media track has at least one active consumer.
	Used() error
	// Write a frame to this media track.
	//
	// `timestamp_us` is the presentation timestamp in microseconds.
	WriteFrame(payload []byte, timestampUs uint64) error
}
type MoqMediaProducer struct {
	ffiObject FfiObject
}

// Finish this media track and finalize encoding.
func (_self *MoqMediaProducer) Finish() error {
	_pointer := _self.ffiObject.incrementPointer("*MoqMediaProducer")
	defer _self.ffiObject.decrementPointer()
	_, _uniffiErr := rustCallWithError[*MoqError](FfiConverterMoqError{}, func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_moq_ffi_fn_method_moqmediaproducer_finish(
			_pointer, _uniffiStatus)
		return false
	})
	return _uniffiErr.AsError()
}

// Return the name of the media track.
func (_self *MoqMediaProducer) Name() (string, error) {
	_pointer := _self.ffiObject.incrementPointer("*MoqMediaProducer")
	defer _self.ffiObject.decrementPointer()
	_uniffiRV, _uniffiErr := rustCallWithError[*MoqError](FfiConverterMoqError{}, func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_moq_ffi_fn_method_moqmediaproducer_name(
				_pointer, _uniffiStatus),
		}
	})
	if _uniffiErr != nil {
		var _uniffiDefaultValue string
		return _uniffiDefaultValue, _uniffiErr
	} else {
		return FfiConverterStringINSTANCE.Lift(_uniffiRV), nil
	}
}

// Wait until this media track has no active consumers.
func (_self *MoqMediaProducer) Unused() error {
	_pointer := _self.ffiObject.incrementPointer("*MoqMediaProducer")
	defer _self.ffiObject.decrementPointer()
	_, err := uniffiRustCallAsync[*MoqError](
		FfiConverterMoqErrorINSTANCE,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) struct{} {
			C.ffi_moq_ffi_rust_future_complete_void(handle, status)
			return struct{}{}
		},
		// liftFn
		func(_ struct{}) struct{} { return struct{}{} },
		C.uniffi_moq_ffi_fn_method_moqmediaproducer_unused(
			_pointer),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_moq_ffi_rust_future_poll_void(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_moq_ffi_rust_future_free_void(handle)
		},
	)

	if err == nil {
		return nil
	}

	return err
}

// Wait until this media track has at least one active consumer.
func (_self *MoqMediaProducer) Used() error {
	_pointer := _self.ffiObject.incrementPointer("*MoqMediaProducer")
	defer _self.ffiObject.decrementPointer()
	_, err := uniffiRustCallAsync[*MoqError](
		FfiConverterMoqErrorINSTANCE,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) struct{} {
			C.ffi_moq_ffi_rust_future_complete_void(handle, status)
			return struct{}{}
		},
		// liftFn
		func(_ struct{}) struct{} { return struct{}{} },
		C.uniffi_moq_ffi_fn_method_moqmediaproducer_used(
			_pointer),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_moq_ffi_rust_future_poll_void(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_moq_ffi_rust_future_free_void(handle)
		},
	)

	if err == nil {
		return nil
	}

	return err
}

// Write a frame to this media track.
//
// `timestamp_us` is the presentation timestamp in microseconds.
func (_self *MoqMediaProducer) WriteFrame(payload []byte, timestampUs uint64) error {
	_pointer := _self.ffiObject.incrementPointer("*MoqMediaProducer")
	defer _self.ffiObject.decrementPointer()
	_, _uniffiErr := rustCallWithError[*MoqError](FfiConverterMoqError{}, func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_moq_ffi_fn_method_moqmediaproducer_write_frame(
			_pointer, FfiConverterBytesINSTANCE.Lower(payload), FfiConverterUint64INSTANCE.Lower(timestampUs), _uniffiStatus)
		return false
	})
	return _uniffiErr.AsError()
}
func (object *MoqMediaProducer) Destroy() {
	runtime.SetFinalizer(object, nil)
	object.ffiObject.destroy()
}

type FfiConverterMoqMediaProducer struct{}

var FfiConverterMoqMediaProducerINSTANCE = FfiConverterMoqMediaProducer{}

func (c FfiConverterMoqMediaProducer) Lift(handle C.uint64_t) *MoqMediaProducer {
	result := &MoqMediaProducer{
		newFfiObject(
			handle,
			func(handle C.uint64_t, status *C.RustCallStatus) C.uint64_t {
				return C.uniffi_moq_ffi_fn_clone_moqmediaproducer(handle, status)
			},
			func(handle C.uint64_t, status *C.RustCallStatus) {
				C.uniffi_moq_ffi_fn_free_moqmediaproducer(handle, status)
			},
		),
	}
	runtime.SetFinalizer(result, (*MoqMediaProducer).Destroy)
	return result
}

func (c FfiConverterMoqMediaProducer) Read(reader io.Reader) *MoqMediaProducer {
	return c.Lift(C.uint64_t(readUint64(reader)))
}

func (c FfiConverterMoqMediaProducer) Lower(value *MoqMediaProducer) C.uint64_t {
	// TODO: this is bad - all synchronization from ObjectRuntime.go is discarded here,
	// because the handle will be decremented immediately after this function returns,
	// and someone will be left holding onto a non-locked handle.
	handle := value.ffiObject.incrementPointer("*MoqMediaProducer")
	defer value.ffiObject.decrementPointer()
	return handle
}

func (c FfiConverterMoqMediaProducer) Write(writer io.Writer, value *MoqMediaProducer) {
	writeUint64(writer, uint64(c.Lower(value)))
}

func LiftFromExternalMoqMediaProducer(handle uint64) *MoqMediaProducer {
	return FfiConverterMoqMediaProducerINSTANCE.Lift(C.uint64_t(handle))
}

func LowerToExternalMoqMediaProducer(value *MoqMediaProducer) uint64 {
	return uint64(FfiConverterMoqMediaProducerINSTANCE.Lower(value))
}

type FfiDestroyerMoqMediaProducer struct{}

func (_ FfiDestroyerMoqMediaProducer) Destroy(value *MoqMediaProducer) {
	value.Destroy()
}

type MoqOriginConsumerInterface interface {
	// Subscribe to all broadcast announcements under a prefix.
	Announced(prefix string) (*MoqAnnounced, error)
	// Wait for a specific broadcast to be announced by path.
	AnnouncedBroadcast(path string) (*MoqAnnouncedBroadcast, error)
}
type MoqOriginConsumer struct {
	ffiObject FfiObject
}

// Subscribe to all broadcast announcements under a prefix.
func (_self *MoqOriginConsumer) Announced(prefix string) (*MoqAnnounced, error) {
	_pointer := _self.ffiObject.incrementPointer("*MoqOriginConsumer")
	defer _self.ffiObject.decrementPointer()
	_uniffiRV, _uniffiErr := rustCallWithError[*MoqError](FfiConverterMoqError{}, func(_uniffiStatus *C.RustCallStatus) C.uint64_t {
		return C.uniffi_moq_ffi_fn_method_moqoriginconsumer_announced(
			_pointer, FfiConverterStringINSTANCE.Lower(prefix), _uniffiStatus)
	})
	if _uniffiErr != nil {
		var _uniffiDefaultValue *MoqAnnounced
		return _uniffiDefaultValue, _uniffiErr
	} else {
		return FfiConverterMoqAnnouncedINSTANCE.Lift(_uniffiRV), nil
	}
}

// Wait for a specific broadcast to be announced by path.
func (_self *MoqOriginConsumer) AnnouncedBroadcast(path string) (*MoqAnnouncedBroadcast, error) {
	_pointer := _self.ffiObject.incrementPointer("*MoqOriginConsumer")
	defer _self.ffiObject.decrementPointer()
	_uniffiRV, _uniffiErr := rustCallWithError[*MoqError](FfiConverterMoqError{}, func(_uniffiStatus *C.RustCallStatus) C.uint64_t {
		return C.uniffi_moq_ffi_fn_method_moqoriginconsumer_announced_broadcast(
			_pointer, FfiConverterStringINSTANCE.Lower(path), _uniffiStatus)
	})
	if _uniffiErr != nil {
		var _uniffiDefaultValue *MoqAnnouncedBroadcast
		return _uniffiDefaultValue, _uniffiErr
	} else {
		return FfiConverterMoqAnnouncedBroadcastINSTANCE.Lift(_uniffiRV), nil
	}
}
func (object *MoqOriginConsumer) Destroy() {
	runtime.SetFinalizer(object, nil)
	object.ffiObject.destroy()
}

type FfiConverterMoqOriginConsumer struct{}

var FfiConverterMoqOriginConsumerINSTANCE = FfiConverterMoqOriginConsumer{}

func (c FfiConverterMoqOriginConsumer) Lift(handle C.uint64_t) *MoqOriginConsumer {
	result := &MoqOriginConsumer{
		newFfiObject(
			handle,
			func(handle C.uint64_t, status *C.RustCallStatus) C.uint64_t {
				return C.uniffi_moq_ffi_fn_clone_moqoriginconsumer(handle, status)
			},
			func(handle C.uint64_t, status *C.RustCallStatus) {
				C.uniffi_moq_ffi_fn_free_moqoriginconsumer(handle, status)
			},
		),
	}
	runtime.SetFinalizer(result, (*MoqOriginConsumer).Destroy)
	return result
}

func (c FfiConverterMoqOriginConsumer) Read(reader io.Reader) *MoqOriginConsumer {
	return c.Lift(C.uint64_t(readUint64(reader)))
}

func (c FfiConverterMoqOriginConsumer) Lower(value *MoqOriginConsumer) C.uint64_t {
	// TODO: this is bad - all synchronization from ObjectRuntime.go is discarded here,
	// because the handle will be decremented immediately after this function returns,
	// and someone will be left holding onto a non-locked handle.
	handle := value.ffiObject.incrementPointer("*MoqOriginConsumer")
	defer value.ffiObject.decrementPointer()
	return handle
}

func (c FfiConverterMoqOriginConsumer) Write(writer io.Writer, value *MoqOriginConsumer) {
	writeUint64(writer, uint64(c.Lower(value)))
}

func LiftFromExternalMoqOriginConsumer(handle uint64) *MoqOriginConsumer {
	return FfiConverterMoqOriginConsumerINSTANCE.Lift(C.uint64_t(handle))
}

func LowerToExternalMoqOriginConsumer(value *MoqOriginConsumer) uint64 {
	return uint64(FfiConverterMoqOriginConsumerINSTANCE.Lower(value))
}

type FfiDestroyerMoqOriginConsumer struct{}

func (_ FfiDestroyerMoqOriginConsumer) Destroy(value *MoqOriginConsumer) {
	value.Destroy()
}

type MoqOriginProducerInterface interface {
	// Create a consumer for this origin.
	Consume() *MoqOriginConsumer
	// Publish a broadcast to this origin under the given path.
	Publish(path string, broadcast *MoqBroadcastProducer) error
}
type MoqOriginProducer struct {
	ffiObject FfiObject
}

// Create a new origin for publishing and/or consuming broadcasts.
func NewMoqOriginProducer() *MoqOriginProducer {
	return FfiConverterMoqOriginProducerINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint64_t {
		return C.uniffi_moq_ffi_fn_constructor_moqoriginproducer_new(_uniffiStatus)
	}))
}

// Create a consumer for this origin.
func (_self *MoqOriginProducer) Consume() *MoqOriginConsumer {
	_pointer := _self.ffiObject.incrementPointer("*MoqOriginProducer")
	defer _self.ffiObject.decrementPointer()
	return FfiConverterMoqOriginConsumerINSTANCE.Lift(rustCall(func(_uniffiStatus *C.RustCallStatus) C.uint64_t {
		return C.uniffi_moq_ffi_fn_method_moqoriginproducer_consume(
			_pointer, _uniffiStatus)
	}))
}

// Publish a broadcast to this origin under the given path.
func (_self *MoqOriginProducer) Publish(path string, broadcast *MoqBroadcastProducer) error {
	_pointer := _self.ffiObject.incrementPointer("*MoqOriginProducer")
	defer _self.ffiObject.decrementPointer()
	_, _uniffiErr := rustCallWithError[*MoqError](FfiConverterMoqError{}, func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_moq_ffi_fn_method_moqoriginproducer_publish(
			_pointer, FfiConverterStringINSTANCE.Lower(path), FfiConverterMoqBroadcastProducerINSTANCE.Lower(broadcast), _uniffiStatus)
		return false
	})
	return _uniffiErr.AsError()
}
func (object *MoqOriginProducer) Destroy() {
	runtime.SetFinalizer(object, nil)
	object.ffiObject.destroy()
}

type FfiConverterMoqOriginProducer struct{}

var FfiConverterMoqOriginProducerINSTANCE = FfiConverterMoqOriginProducer{}

func (c FfiConverterMoqOriginProducer) Lift(handle C.uint64_t) *MoqOriginProducer {
	result := &MoqOriginProducer{
		newFfiObject(
			handle,
			func(handle C.uint64_t, status *C.RustCallStatus) C.uint64_t {
				return C.uniffi_moq_ffi_fn_clone_moqoriginproducer(handle, status)
			},
			func(handle C.uint64_t, status *C.RustCallStatus) {
				C.uniffi_moq_ffi_fn_free_moqoriginproducer(handle, status)
			},
		),
	}
	runtime.SetFinalizer(result, (*MoqOriginProducer).Destroy)
	return result
}

func (c FfiConverterMoqOriginProducer) Read(reader io.Reader) *MoqOriginProducer {
	return c.Lift(C.uint64_t(readUint64(reader)))
}

func (c FfiConverterMoqOriginProducer) Lower(value *MoqOriginProducer) C.uint64_t {
	// TODO: this is bad - all synchronization from ObjectRuntime.go is discarded here,
	// because the handle will be decremented immediately after this function returns,
	// and someone will be left holding onto a non-locked handle.
	handle := value.ffiObject.incrementPointer("*MoqOriginProducer")
	defer value.ffiObject.decrementPointer()
	return handle
}

func (c FfiConverterMoqOriginProducer) Write(writer io.Writer, value *MoqOriginProducer) {
	writeUint64(writer, uint64(c.Lower(value)))
}

func LiftFromExternalMoqOriginProducer(handle uint64) *MoqOriginProducer {
	return FfiConverterMoqOriginProducerINSTANCE.Lift(C.uint64_t(handle))
}

func LowerToExternalMoqOriginProducer(value *MoqOriginProducer) uint64 {
	return uint64(FfiConverterMoqOriginProducerINSTANCE.Lower(value))
}

type FfiDestroyerMoqOriginProducer struct{}

func (_ FfiDestroyerMoqOriginProducer) Destroy(value *MoqOriginProducer) {
	value.Destroy()
}

type MoqSessionInterface interface {
	// Close the session with the given error code.
	Cancel(code uint32)
	// Wait until the session is closed.
	Closed() error
}
type MoqSession struct {
	ffiObject FfiObject
}

// Close the session with the given error code.
func (_self *MoqSession) Cancel(code uint32) {
	_pointer := _self.ffiObject.incrementPointer("*MoqSession")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_moq_ffi_fn_method_moqsession_cancel(
			_pointer, FfiConverterUint32INSTANCE.Lower(code), _uniffiStatus)
		return false
	})
}

// Wait until the session is closed.
func (_self *MoqSession) Closed() error {
	_pointer := _self.ffiObject.incrementPointer("*MoqSession")
	defer _self.ffiObject.decrementPointer()
	_, err := uniffiRustCallAsync[*MoqError](
		FfiConverterMoqErrorINSTANCE,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) struct{} {
			C.ffi_moq_ffi_rust_future_complete_void(handle, status)
			return struct{}{}
		},
		// liftFn
		func(_ struct{}) struct{} { return struct{}{} },
		C.uniffi_moq_ffi_fn_method_moqsession_closed(
			_pointer),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_moq_ffi_rust_future_poll_void(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_moq_ffi_rust_future_free_void(handle)
		},
	)

	if err == nil {
		return nil
	}

	return err
}
func (object *MoqSession) Destroy() {
	runtime.SetFinalizer(object, nil)
	object.ffiObject.destroy()
}

type FfiConverterMoqSession struct{}

var FfiConverterMoqSessionINSTANCE = FfiConverterMoqSession{}

func (c FfiConverterMoqSession) Lift(handle C.uint64_t) *MoqSession {
	result := &MoqSession{
		newFfiObject(
			handle,
			func(handle C.uint64_t, status *C.RustCallStatus) C.uint64_t {
				return C.uniffi_moq_ffi_fn_clone_moqsession(handle, status)
			},
			func(handle C.uint64_t, status *C.RustCallStatus) {
				C.uniffi_moq_ffi_fn_free_moqsession(handle, status)
			},
		),
	}
	runtime.SetFinalizer(result, (*MoqSession).Destroy)
	return result
}

func (c FfiConverterMoqSession) Read(reader io.Reader) *MoqSession {
	return c.Lift(C.uint64_t(readUint64(reader)))
}

func (c FfiConverterMoqSession) Lower(value *MoqSession) C.uint64_t {
	// TODO: this is bad - all synchronization from ObjectRuntime.go is discarded here,
	// because the handle will be decremented immediately after this function returns,
	// and someone will be left holding onto a non-locked handle.
	handle := value.ffiObject.incrementPointer("*MoqSession")
	defer value.ffiObject.decrementPointer()
	return handle
}

func (c FfiConverterMoqSession) Write(writer io.Writer, value *MoqSession) {
	writeUint64(writer, uint64(c.Lower(value)))
}

func LiftFromExternalMoqSession(handle uint64) *MoqSession {
	return FfiConverterMoqSessionINSTANCE.Lift(C.uint64_t(handle))
}

func LowerToExternalMoqSession(value *MoqSession) uint64 {
	return uint64(FfiConverterMoqSessionINSTANCE.Lower(value))
}

type FfiDestroyerMoqSession struct{}

func (_ FfiDestroyerMoqSession) Destroy(value *MoqSession) {
	value.Destroy()
}

type MoqTrackConsumerInterface interface {
	Cancel()
	// Return the next group in sequence order, skipping forward if the reader
	// has fallen behind. Returns `None` when the track ends.
	NextGroup() (**MoqGroupConsumer, error)
	// Read the first frame of the next group.
	//
	// Convenience for tracks using one-frame-per-group (like moq-boy's
	// status/command tracks). Returns `None` when the track ends.
	ReadFrame() (*[]byte, error)
	// Return the next group in arrival order. Returns `None` when the track ends.
	//
	// Groups are returned as they arrive on the wire, which may be out of sequence
	// order (e.g. if a later group lands before an earlier one on a separate stream).
	RecvGroup() (**MoqGroupConsumer, error)
}
type MoqTrackConsumer struct {
	ffiObject FfiObject
}

func (_self *MoqTrackConsumer) Cancel() {
	_pointer := _self.ffiObject.incrementPointer("*MoqTrackConsumer")
	defer _self.ffiObject.decrementPointer()
	rustCall(func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_moq_ffi_fn_method_moqtrackconsumer_cancel(
			_pointer, _uniffiStatus)
		return false
	})
}

// Return the next group in sequence order, skipping forward if the reader
// has fallen behind. Returns `None` when the track ends.
func (_self *MoqTrackConsumer) NextGroup() (**MoqGroupConsumer, error) {
	_pointer := _self.ffiObject.incrementPointer("*MoqTrackConsumer")
	defer _self.ffiObject.decrementPointer()
	res, err := uniffiRustCallAsync[*MoqError](
		FfiConverterMoqErrorINSTANCE,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) RustBufferI {
			res := C.ffi_moq_ffi_rust_future_complete_rust_buffer(handle, status)
			return GoRustBuffer{
				inner: res,
			}
		},
		// liftFn
		func(ffi RustBufferI) **MoqGroupConsumer {
			return FfiConverterOptionalMoqGroupConsumerINSTANCE.Lift(ffi)
		},
		C.uniffi_moq_ffi_fn_method_moqtrackconsumer_next_group(
			_pointer),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_moq_ffi_rust_future_poll_rust_buffer(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_moq_ffi_rust_future_free_rust_buffer(handle)
		},
	)

	if err == nil {
		return res, nil
	}

	return res, err
}

// Read the first frame of the next group.
//
// Convenience for tracks using one-frame-per-group (like moq-boy's
// status/command tracks). Returns `None` when the track ends.
func (_self *MoqTrackConsumer) ReadFrame() (*[]byte, error) {
	_pointer := _self.ffiObject.incrementPointer("*MoqTrackConsumer")
	defer _self.ffiObject.decrementPointer()
	res, err := uniffiRustCallAsync[*MoqError](
		FfiConverterMoqErrorINSTANCE,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) RustBufferI {
			res := C.ffi_moq_ffi_rust_future_complete_rust_buffer(handle, status)
			return GoRustBuffer{
				inner: res,
			}
		},
		// liftFn
		func(ffi RustBufferI) *[]byte {
			return FfiConverterOptionalBytesINSTANCE.Lift(ffi)
		},
		C.uniffi_moq_ffi_fn_method_moqtrackconsumer_read_frame(
			_pointer),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_moq_ffi_rust_future_poll_rust_buffer(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_moq_ffi_rust_future_free_rust_buffer(handle)
		},
	)

	if err == nil {
		return res, nil
	}

	return res, err
}

// Return the next group in arrival order. Returns `None` when the track ends.
//
// Groups are returned as they arrive on the wire, which may be out of sequence
// order (e.g. if a later group lands before an earlier one on a separate stream).
func (_self *MoqTrackConsumer) RecvGroup() (**MoqGroupConsumer, error) {
	_pointer := _self.ffiObject.incrementPointer("*MoqTrackConsumer")
	defer _self.ffiObject.decrementPointer()
	res, err := uniffiRustCallAsync[*MoqError](
		FfiConverterMoqErrorINSTANCE,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) RustBufferI {
			res := C.ffi_moq_ffi_rust_future_complete_rust_buffer(handle, status)
			return GoRustBuffer{
				inner: res,
			}
		},
		// liftFn
		func(ffi RustBufferI) **MoqGroupConsumer {
			return FfiConverterOptionalMoqGroupConsumerINSTANCE.Lift(ffi)
		},
		C.uniffi_moq_ffi_fn_method_moqtrackconsumer_recv_group(
			_pointer),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_moq_ffi_rust_future_poll_rust_buffer(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_moq_ffi_rust_future_free_rust_buffer(handle)
		},
	)

	if err == nil {
		return res, nil
	}

	return res, err
}
func (object *MoqTrackConsumer) Destroy() {
	runtime.SetFinalizer(object, nil)
	object.ffiObject.destroy()
}

type FfiConverterMoqTrackConsumer struct{}

var FfiConverterMoqTrackConsumerINSTANCE = FfiConverterMoqTrackConsumer{}

func (c FfiConverterMoqTrackConsumer) Lift(handle C.uint64_t) *MoqTrackConsumer {
	result := &MoqTrackConsumer{
		newFfiObject(
			handle,
			func(handle C.uint64_t, status *C.RustCallStatus) C.uint64_t {
				return C.uniffi_moq_ffi_fn_clone_moqtrackconsumer(handle, status)
			},
			func(handle C.uint64_t, status *C.RustCallStatus) {
				C.uniffi_moq_ffi_fn_free_moqtrackconsumer(handle, status)
			},
		),
	}
	runtime.SetFinalizer(result, (*MoqTrackConsumer).Destroy)
	return result
}

func (c FfiConverterMoqTrackConsumer) Read(reader io.Reader) *MoqTrackConsumer {
	return c.Lift(C.uint64_t(readUint64(reader)))
}

func (c FfiConverterMoqTrackConsumer) Lower(value *MoqTrackConsumer) C.uint64_t {
	// TODO: this is bad - all synchronization from ObjectRuntime.go is discarded here,
	// because the handle will be decremented immediately after this function returns,
	// and someone will be left holding onto a non-locked handle.
	handle := value.ffiObject.incrementPointer("*MoqTrackConsumer")
	defer value.ffiObject.decrementPointer()
	return handle
}

func (c FfiConverterMoqTrackConsumer) Write(writer io.Writer, value *MoqTrackConsumer) {
	writeUint64(writer, uint64(c.Lower(value)))
}

func LiftFromExternalMoqTrackConsumer(handle uint64) *MoqTrackConsumer {
	return FfiConverterMoqTrackConsumerINSTANCE.Lift(C.uint64_t(handle))
}

func LowerToExternalMoqTrackConsumer(value *MoqTrackConsumer) uint64 {
	return uint64(FfiConverterMoqTrackConsumerINSTANCE.Lower(value))
}

type FfiDestroyerMoqTrackConsumer struct{}

func (_ FfiDestroyerMoqTrackConsumer) Destroy(value *MoqTrackConsumer) {
	value.Destroy()
}

type MoqTrackProducerInterface interface {
	// Append a new group to the track, returning a producer for writing frames into it.
	AppendGroup() (*MoqGroupProducer, error)
	// Create a consumer that reads from this producer's track.
	//
	// Useful for local pub/sub without going through an origin/broadcast.
	Consume() (*MoqTrackConsumer, error)
	Finish() error
	// Return the name of this track.
	Name() (string, error)
	// Wait until this track has no active consumers.
	Unused() error
	// Wait until this track has at least one active consumer.
	Used() error
	// Convenience: write a single-frame group in one call — the same pattern
	// used by moq-boy's status/command tracks.
	WriteFrame(payload []byte) error
}
type MoqTrackProducer struct {
	ffiObject FfiObject
}

// Append a new group to the track, returning a producer for writing frames into it.
func (_self *MoqTrackProducer) AppendGroup() (*MoqGroupProducer, error) {
	_pointer := _self.ffiObject.incrementPointer("*MoqTrackProducer")
	defer _self.ffiObject.decrementPointer()
	_uniffiRV, _uniffiErr := rustCallWithError[*MoqError](FfiConverterMoqError{}, func(_uniffiStatus *C.RustCallStatus) C.uint64_t {
		return C.uniffi_moq_ffi_fn_method_moqtrackproducer_append_group(
			_pointer, _uniffiStatus)
	})
	if _uniffiErr != nil {
		var _uniffiDefaultValue *MoqGroupProducer
		return _uniffiDefaultValue, _uniffiErr
	} else {
		return FfiConverterMoqGroupProducerINSTANCE.Lift(_uniffiRV), nil
	}
}

// Create a consumer that reads from this producer's track.
//
// Useful for local pub/sub without going through an origin/broadcast.
func (_self *MoqTrackProducer) Consume() (*MoqTrackConsumer, error) {
	_pointer := _self.ffiObject.incrementPointer("*MoqTrackProducer")
	defer _self.ffiObject.decrementPointer()
	_uniffiRV, _uniffiErr := rustCallWithError[*MoqError](FfiConverterMoqError{}, func(_uniffiStatus *C.RustCallStatus) C.uint64_t {
		return C.uniffi_moq_ffi_fn_method_moqtrackproducer_consume(
			_pointer, _uniffiStatus)
	})
	if _uniffiErr != nil {
		var _uniffiDefaultValue *MoqTrackConsumer
		return _uniffiDefaultValue, _uniffiErr
	} else {
		return FfiConverterMoqTrackConsumerINSTANCE.Lift(_uniffiRV), nil
	}
}

func (_self *MoqTrackProducer) Finish() error {
	_pointer := _self.ffiObject.incrementPointer("*MoqTrackProducer")
	defer _self.ffiObject.decrementPointer()
	_, _uniffiErr := rustCallWithError[*MoqError](FfiConverterMoqError{}, func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_moq_ffi_fn_method_moqtrackproducer_finish(
			_pointer, _uniffiStatus)
		return false
	})
	return _uniffiErr.AsError()
}

// Return the name of this track.
func (_self *MoqTrackProducer) Name() (string, error) {
	_pointer := _self.ffiObject.incrementPointer("*MoqTrackProducer")
	defer _self.ffiObject.decrementPointer()
	_uniffiRV, _uniffiErr := rustCallWithError[*MoqError](FfiConverterMoqError{}, func(_uniffiStatus *C.RustCallStatus) RustBufferI {
		return GoRustBuffer{
			inner: C.uniffi_moq_ffi_fn_method_moqtrackproducer_name(
				_pointer, _uniffiStatus),
		}
	})
	if _uniffiErr != nil {
		var _uniffiDefaultValue string
		return _uniffiDefaultValue, _uniffiErr
	} else {
		return FfiConverterStringINSTANCE.Lift(_uniffiRV), nil
	}
}

// Wait until this track has no active consumers.
func (_self *MoqTrackProducer) Unused() error {
	_pointer := _self.ffiObject.incrementPointer("*MoqTrackProducer")
	defer _self.ffiObject.decrementPointer()
	_, err := uniffiRustCallAsync[*MoqError](
		FfiConverterMoqErrorINSTANCE,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) struct{} {
			C.ffi_moq_ffi_rust_future_complete_void(handle, status)
			return struct{}{}
		},
		// liftFn
		func(_ struct{}) struct{} { return struct{}{} },
		C.uniffi_moq_ffi_fn_method_moqtrackproducer_unused(
			_pointer),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_moq_ffi_rust_future_poll_void(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_moq_ffi_rust_future_free_void(handle)
		},
	)

	if err == nil {
		return nil
	}

	return err
}

// Wait until this track has at least one active consumer.
func (_self *MoqTrackProducer) Used() error {
	_pointer := _self.ffiObject.incrementPointer("*MoqTrackProducer")
	defer _self.ffiObject.decrementPointer()
	_, err := uniffiRustCallAsync[*MoqError](
		FfiConverterMoqErrorINSTANCE,
		// completeFn
		func(handle C.uint64_t, status *C.RustCallStatus) struct{} {
			C.ffi_moq_ffi_rust_future_complete_void(handle, status)
			return struct{}{}
		},
		// liftFn
		func(_ struct{}) struct{} { return struct{}{} },
		C.uniffi_moq_ffi_fn_method_moqtrackproducer_used(
			_pointer),
		// pollFn
		func(handle C.uint64_t, continuation C.UniffiRustFutureContinuationCallback, data C.uint64_t) {
			C.ffi_moq_ffi_rust_future_poll_void(handle, continuation, data)
		},
		// freeFn
		func(handle C.uint64_t) {
			C.ffi_moq_ffi_rust_future_free_void(handle)
		},
	)

	if err == nil {
		return nil
	}

	return err
}

// Convenience: write a single-frame group in one call — the same pattern
// used by moq-boy's status/command tracks.
func (_self *MoqTrackProducer) WriteFrame(payload []byte) error {
	_pointer := _self.ffiObject.incrementPointer("*MoqTrackProducer")
	defer _self.ffiObject.decrementPointer()
	_, _uniffiErr := rustCallWithError[*MoqError](FfiConverterMoqError{}, func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_moq_ffi_fn_method_moqtrackproducer_write_frame(
			_pointer, FfiConverterBytesINSTANCE.Lower(payload), _uniffiStatus)
		return false
	})
	return _uniffiErr.AsError()
}
func (object *MoqTrackProducer) Destroy() {
	runtime.SetFinalizer(object, nil)
	object.ffiObject.destroy()
}

type FfiConverterMoqTrackProducer struct{}

var FfiConverterMoqTrackProducerINSTANCE = FfiConverterMoqTrackProducer{}

func (c FfiConverterMoqTrackProducer) Lift(handle C.uint64_t) *MoqTrackProducer {
	result := &MoqTrackProducer{
		newFfiObject(
			handle,
			func(handle C.uint64_t, status *C.RustCallStatus) C.uint64_t {
				return C.uniffi_moq_ffi_fn_clone_moqtrackproducer(handle, status)
			},
			func(handle C.uint64_t, status *C.RustCallStatus) {
				C.uniffi_moq_ffi_fn_free_moqtrackproducer(handle, status)
			},
		),
	}
	runtime.SetFinalizer(result, (*MoqTrackProducer).Destroy)
	return result
}

func (c FfiConverterMoqTrackProducer) Read(reader io.Reader) *MoqTrackProducer {
	return c.Lift(C.uint64_t(readUint64(reader)))
}

func (c FfiConverterMoqTrackProducer) Lower(value *MoqTrackProducer) C.uint64_t {
	// TODO: this is bad - all synchronization from ObjectRuntime.go is discarded here,
	// because the handle will be decremented immediately after this function returns,
	// and someone will be left holding onto a non-locked handle.
	handle := value.ffiObject.incrementPointer("*MoqTrackProducer")
	defer value.ffiObject.decrementPointer()
	return handle
}

func (c FfiConverterMoqTrackProducer) Write(writer io.Writer, value *MoqTrackProducer) {
	writeUint64(writer, uint64(c.Lower(value)))
}

func LiftFromExternalMoqTrackProducer(handle uint64) *MoqTrackProducer {
	return FfiConverterMoqTrackProducerINSTANCE.Lift(C.uint64_t(handle))
}

func LowerToExternalMoqTrackProducer(value *MoqTrackProducer) uint64 {
	return uint64(FfiConverterMoqTrackProducerINSTANCE.Lower(value))
}

type FfiDestroyerMoqTrackProducer struct{}

func (_ FfiDestroyerMoqTrackProducer) Destroy(value *MoqTrackProducer) {
	value.Destroy()
}

type MoqAudio struct {
	Codec        string
	Description  *[]byte
	SampleRate   uint32
	ChannelCount uint32
	Bitrate      *uint64
	Container    Container
}

func (r *MoqAudio) Destroy() {
	FfiDestroyerString{}.Destroy(r.Codec)
	FfiDestroyerOptionalBytes{}.Destroy(r.Description)
	FfiDestroyerUint32{}.Destroy(r.SampleRate)
	FfiDestroyerUint32{}.Destroy(r.ChannelCount)
	FfiDestroyerOptionalUint64{}.Destroy(r.Bitrate)
	FfiDestroyerContainer{}.Destroy(r.Container)
}

type FfiConverterMoqAudio struct{}

var FfiConverterMoqAudioINSTANCE = FfiConverterMoqAudio{}

func (c FfiConverterMoqAudio) Lift(rb RustBufferI) MoqAudio {
	return LiftFromRustBuffer[MoqAudio](c, rb)
}

func (c FfiConverterMoqAudio) Read(reader io.Reader) MoqAudio {
	return MoqAudio{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterOptionalBytesINSTANCE.Read(reader),
		FfiConverterUint32INSTANCE.Read(reader),
		FfiConverterUint32INSTANCE.Read(reader),
		FfiConverterOptionalUint64INSTANCE.Read(reader),
		FfiConverterContainerINSTANCE.Read(reader),
	}
}

func (c FfiConverterMoqAudio) Lower(value MoqAudio) C.RustBuffer {
	return LowerIntoRustBuffer[MoqAudio](c, value)
}

func (c FfiConverterMoqAudio) LowerExternal(value MoqAudio) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[MoqAudio](c, value))
}

func (c FfiConverterMoqAudio) Write(writer io.Writer, value MoqAudio) {
	FfiConverterStringINSTANCE.Write(writer, value.Codec)
	FfiConverterOptionalBytesINSTANCE.Write(writer, value.Description)
	FfiConverterUint32INSTANCE.Write(writer, value.SampleRate)
	FfiConverterUint32INSTANCE.Write(writer, value.ChannelCount)
	FfiConverterOptionalUint64INSTANCE.Write(writer, value.Bitrate)
	FfiConverterContainerINSTANCE.Write(writer, value.Container)
}

type FfiDestroyerMoqAudio struct{}

func (_ FfiDestroyerMoqAudio) Destroy(value MoqAudio) {
	value.Destroy()
}

type MoqCatalog struct {
	Video    map[string]MoqVideo
	Audio    map[string]MoqAudio
	Display  *MoqDimensions
	Rotation *float64
	Flip     *bool
}

func (r *MoqCatalog) Destroy() {
	FfiDestroyerMapStringMoqVideo{}.Destroy(r.Video)
	FfiDestroyerMapStringMoqAudio{}.Destroy(r.Audio)
	FfiDestroyerOptionalMoqDimensions{}.Destroy(r.Display)
	FfiDestroyerOptionalFloat64{}.Destroy(r.Rotation)
	FfiDestroyerOptionalBool{}.Destroy(r.Flip)
}

type FfiConverterMoqCatalog struct{}

var FfiConverterMoqCatalogINSTANCE = FfiConverterMoqCatalog{}

func (c FfiConverterMoqCatalog) Lift(rb RustBufferI) MoqCatalog {
	return LiftFromRustBuffer[MoqCatalog](c, rb)
}

func (c FfiConverterMoqCatalog) Read(reader io.Reader) MoqCatalog {
	return MoqCatalog{
		FfiConverterMapStringMoqVideoINSTANCE.Read(reader),
		FfiConverterMapStringMoqAudioINSTANCE.Read(reader),
		FfiConverterOptionalMoqDimensionsINSTANCE.Read(reader),
		FfiConverterOptionalFloat64INSTANCE.Read(reader),
		FfiConverterOptionalBoolINSTANCE.Read(reader),
	}
}

func (c FfiConverterMoqCatalog) Lower(value MoqCatalog) C.RustBuffer {
	return LowerIntoRustBuffer[MoqCatalog](c, value)
}

func (c FfiConverterMoqCatalog) LowerExternal(value MoqCatalog) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[MoqCatalog](c, value))
}

func (c FfiConverterMoqCatalog) Write(writer io.Writer, value MoqCatalog) {
	FfiConverterMapStringMoqVideoINSTANCE.Write(writer, value.Video)
	FfiConverterMapStringMoqAudioINSTANCE.Write(writer, value.Audio)
	FfiConverterOptionalMoqDimensionsINSTANCE.Write(writer, value.Display)
	FfiConverterOptionalFloat64INSTANCE.Write(writer, value.Rotation)
	FfiConverterOptionalBoolINSTANCE.Write(writer, value.Flip)
}

type FfiDestroyerMoqCatalog struct{}

func (_ FfiDestroyerMoqCatalog) Destroy(value MoqCatalog) {
	value.Destroy()
}

type MoqDimensions struct {
	Width  uint32
	Height uint32
}

func (r *MoqDimensions) Destroy() {
	FfiDestroyerUint32{}.Destroy(r.Width)
	FfiDestroyerUint32{}.Destroy(r.Height)
}

type FfiConverterMoqDimensions struct{}

var FfiConverterMoqDimensionsINSTANCE = FfiConverterMoqDimensions{}

func (c FfiConverterMoqDimensions) Lift(rb RustBufferI) MoqDimensions {
	return LiftFromRustBuffer[MoqDimensions](c, rb)
}

func (c FfiConverterMoqDimensions) Read(reader io.Reader) MoqDimensions {
	return MoqDimensions{
		FfiConverterUint32INSTANCE.Read(reader),
		FfiConverterUint32INSTANCE.Read(reader),
	}
}

func (c FfiConverterMoqDimensions) Lower(value MoqDimensions) C.RustBuffer {
	return LowerIntoRustBuffer[MoqDimensions](c, value)
}

func (c FfiConverterMoqDimensions) LowerExternal(value MoqDimensions) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[MoqDimensions](c, value))
}

func (c FfiConverterMoqDimensions) Write(writer io.Writer, value MoqDimensions) {
	FfiConverterUint32INSTANCE.Write(writer, value.Width)
	FfiConverterUint32INSTANCE.Write(writer, value.Height)
}

type FfiDestroyerMoqDimensions struct{}

func (_ FfiDestroyerMoqDimensions) Destroy(value MoqDimensions) {
	value.Destroy()
}

// A media frame.
type MoqFrame struct {
	Payload     []byte
	TimestampUs uint64
	Keyframe    bool
}

func (r *MoqFrame) Destroy() {
	FfiDestroyerBytes{}.Destroy(r.Payload)
	FfiDestroyerUint64{}.Destroy(r.TimestampUs)
	FfiDestroyerBool{}.Destroy(r.Keyframe)
}

type FfiConverterMoqFrame struct{}

var FfiConverterMoqFrameINSTANCE = FfiConverterMoqFrame{}

func (c FfiConverterMoqFrame) Lift(rb RustBufferI) MoqFrame {
	return LiftFromRustBuffer[MoqFrame](c, rb)
}

func (c FfiConverterMoqFrame) Read(reader io.Reader) MoqFrame {
	return MoqFrame{
		FfiConverterBytesINSTANCE.Read(reader),
		FfiConverterUint64INSTANCE.Read(reader),
		FfiConverterBoolINSTANCE.Read(reader),
	}
}

func (c FfiConverterMoqFrame) Lower(value MoqFrame) C.RustBuffer {
	return LowerIntoRustBuffer[MoqFrame](c, value)
}

func (c FfiConverterMoqFrame) LowerExternal(value MoqFrame) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[MoqFrame](c, value))
}

func (c FfiConverterMoqFrame) Write(writer io.Writer, value MoqFrame) {
	FfiConverterBytesINSTANCE.Write(writer, value.Payload)
	FfiConverterUint64INSTANCE.Write(writer, value.TimestampUs)
	FfiConverterBoolINSTANCE.Write(writer, value.Keyframe)
}

type FfiDestroyerMoqFrame struct{}

func (_ FfiDestroyerMoqFrame) Destroy(value MoqFrame) {
	value.Destroy()
}

type MoqVideo struct {
	Codec        string
	Description  *[]byte
	Coded        *MoqDimensions
	DisplayRatio *MoqDimensions
	Bitrate      *uint64
	Framerate    *float64
	Container    Container
}

func (r *MoqVideo) Destroy() {
	FfiDestroyerString{}.Destroy(r.Codec)
	FfiDestroyerOptionalBytes{}.Destroy(r.Description)
	FfiDestroyerOptionalMoqDimensions{}.Destroy(r.Coded)
	FfiDestroyerOptionalMoqDimensions{}.Destroy(r.DisplayRatio)
	FfiDestroyerOptionalUint64{}.Destroy(r.Bitrate)
	FfiDestroyerOptionalFloat64{}.Destroy(r.Framerate)
	FfiDestroyerContainer{}.Destroy(r.Container)
}

type FfiConverterMoqVideo struct{}

var FfiConverterMoqVideoINSTANCE = FfiConverterMoqVideo{}

func (c FfiConverterMoqVideo) Lift(rb RustBufferI) MoqVideo {
	return LiftFromRustBuffer[MoqVideo](c, rb)
}

func (c FfiConverterMoqVideo) Read(reader io.Reader) MoqVideo {
	return MoqVideo{
		FfiConverterStringINSTANCE.Read(reader),
		FfiConverterOptionalBytesINSTANCE.Read(reader),
		FfiConverterOptionalMoqDimensionsINSTANCE.Read(reader),
		FfiConverterOptionalMoqDimensionsINSTANCE.Read(reader),
		FfiConverterOptionalUint64INSTANCE.Read(reader),
		FfiConverterOptionalFloat64INSTANCE.Read(reader),
		FfiConverterContainerINSTANCE.Read(reader),
	}
}

func (c FfiConverterMoqVideo) Lower(value MoqVideo) C.RustBuffer {
	return LowerIntoRustBuffer[MoqVideo](c, value)
}

func (c FfiConverterMoqVideo) LowerExternal(value MoqVideo) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[MoqVideo](c, value))
}

func (c FfiConverterMoqVideo) Write(writer io.Writer, value MoqVideo) {
	FfiConverterStringINSTANCE.Write(writer, value.Codec)
	FfiConverterOptionalBytesINSTANCE.Write(writer, value.Description)
	FfiConverterOptionalMoqDimensionsINSTANCE.Write(writer, value.Coded)
	FfiConverterOptionalMoqDimensionsINSTANCE.Write(writer, value.DisplayRatio)
	FfiConverterOptionalUint64INSTANCE.Write(writer, value.Bitrate)
	FfiConverterOptionalFloat64INSTANCE.Write(writer, value.Framerate)
	FfiConverterContainerINSTANCE.Write(writer, value.Container)
}

type FfiDestroyerMoqVideo struct{}

func (_ FfiDestroyerMoqVideo) Destroy(value MoqVideo) {
	value.Destroy()
}

type Container interface {
	Destroy()
}
type ContainerLegacy struct {
}

func (e ContainerLegacy) Destroy() {
}

type ContainerCmaf struct {
	Init []byte
}

func (e ContainerCmaf) Destroy() {
	FfiDestroyerBytes{}.Destroy(e.Init)
}

type FfiConverterContainer struct{}

var FfiConverterContainerINSTANCE = FfiConverterContainer{}

func (c FfiConverterContainer) Lift(rb RustBufferI) Container {
	return LiftFromRustBuffer[Container](c, rb)
}

func (c FfiConverterContainer) Lower(value Container) C.RustBuffer {
	return LowerIntoRustBuffer[Container](c, value)
}

func (c FfiConverterContainer) LowerExternal(value Container) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[Container](c, value))
}
func (FfiConverterContainer) Read(reader io.Reader) Container {
	id := readInt32(reader)
	switch id {
	case 1:
		return ContainerLegacy{}
	case 2:
		return ContainerCmaf{
			FfiConverterBytesINSTANCE.Read(reader),
		}
	default:
		panic(fmt.Sprintf("invalid enum value %v in FfiConverterContainer.Read()", id))
	}
}

func (FfiConverterContainer) Write(writer io.Writer, value Container) {
	switch variant_value := value.(type) {
	case ContainerLegacy:
		writeInt32(writer, 1)
	case ContainerCmaf:
		writeInt32(writer, 2)
		FfiConverterBytesINSTANCE.Write(writer, variant_value.Init)
	default:
		_ = variant_value
		panic(fmt.Sprintf("invalid enum value `%v` in FfiConverterContainer.Write", value))
	}
}

type FfiDestroyerContainer struct{}

func (_ FfiDestroyerContainer) Destroy(value Container) {
	value.Destroy()
}

// Error returned by all UniFFI-exported functions.
type MoqError struct {
	err error
}

// Convenience method to turn *MoqError into error
// Avoiding treating nil pointer as non nil error interface
func (err *MoqError) AsError() error {
	if err == nil {
		return nil
	} else {
		return err
	}
}

func (err MoqError) Error() string {
	return fmt.Sprintf("MoqError: %s", err.err.Error())
}

func (err MoqError) Unwrap() error {
	return err.err
}

// Err* are used for checking error type with `errors.Is`
var ErrMoqErrorProtocol = fmt.Errorf("MoqErrorProtocol")
var ErrMoqErrorMedia = fmt.Errorf("MoqErrorMedia")
var ErrMoqErrorMux = fmt.Errorf("MoqErrorMux")
var ErrMoqErrorUrl = fmt.Errorf("MoqErrorUrl")
var ErrMoqErrorTimeOverflow = fmt.Errorf("MoqErrorTimeOverflow")
var ErrMoqErrorLogLevel = fmt.Errorf("MoqErrorLogLevel")
var ErrMoqErrorTask = fmt.Errorf("MoqErrorTask")
var ErrMoqErrorCancelled = fmt.Errorf("MoqErrorCancelled")
var ErrMoqErrorClosed = fmt.Errorf("MoqErrorClosed")
var ErrMoqErrorConnect = fmt.Errorf("MoqErrorConnect")
var ErrMoqErrorCodec = fmt.Errorf("MoqErrorCodec")
var ErrMoqErrorUnauthorized = fmt.Errorf("MoqErrorUnauthorized")
var ErrMoqErrorLog = fmt.Errorf("MoqErrorLog")

// Variant structs
type MoqErrorProtocol struct {
	message string
}

func NewMoqErrorProtocol() *MoqError {
	return &MoqError{err: &MoqErrorProtocol{}}
}

func (e MoqErrorProtocol) destroy() {
}

func (err MoqErrorProtocol) Error() string {
	return fmt.Sprintf("Protocol: %s", err.message)
}

func (self MoqErrorProtocol) Is(target error) bool {
	return target == ErrMoqErrorProtocol
}

type MoqErrorMedia struct {
	message string
}

func NewMoqErrorMedia() *MoqError {
	return &MoqError{err: &MoqErrorMedia{}}
}

func (e MoqErrorMedia) destroy() {
}

func (err MoqErrorMedia) Error() string {
	return fmt.Sprintf("Media: %s", err.message)
}

func (self MoqErrorMedia) Is(target error) bool {
	return target == ErrMoqErrorMedia
}

type MoqErrorMux struct {
	message string
}

func NewMoqErrorMux() *MoqError {
	return &MoqError{err: &MoqErrorMux{}}
}

func (e MoqErrorMux) destroy() {
}

func (err MoqErrorMux) Error() string {
	return fmt.Sprintf("Mux: %s", err.message)
}

func (self MoqErrorMux) Is(target error) bool {
	return target == ErrMoqErrorMux
}

type MoqErrorUrl struct {
	message string
}

func NewMoqErrorUrl() *MoqError {
	return &MoqError{err: &MoqErrorUrl{}}
}

func (e MoqErrorUrl) destroy() {
}

func (err MoqErrorUrl) Error() string {
	return fmt.Sprintf("Url: %s", err.message)
}

func (self MoqErrorUrl) Is(target error) bool {
	return target == ErrMoqErrorUrl
}

type MoqErrorTimeOverflow struct {
	message string
}

func NewMoqErrorTimeOverflow() *MoqError {
	return &MoqError{err: &MoqErrorTimeOverflow{}}
}

func (e MoqErrorTimeOverflow) destroy() {
}

func (err MoqErrorTimeOverflow) Error() string {
	return fmt.Sprintf("TimeOverflow: %s", err.message)
}

func (self MoqErrorTimeOverflow) Is(target error) bool {
	return target == ErrMoqErrorTimeOverflow
}

type MoqErrorLogLevel struct {
	message string
}

func NewMoqErrorLogLevel() *MoqError {
	return &MoqError{err: &MoqErrorLogLevel{}}
}

func (e MoqErrorLogLevel) destroy() {
}

func (err MoqErrorLogLevel) Error() string {
	return fmt.Sprintf("LogLevel: %s", err.message)
}

func (self MoqErrorLogLevel) Is(target error) bool {
	return target == ErrMoqErrorLogLevel
}

type MoqErrorTask struct {
	message string
}

func NewMoqErrorTask() *MoqError {
	return &MoqError{err: &MoqErrorTask{}}
}

func (e MoqErrorTask) destroy() {
}

func (err MoqErrorTask) Error() string {
	return fmt.Sprintf("Task: %s", err.message)
}

func (self MoqErrorTask) Is(target error) bool {
	return target == ErrMoqErrorTask
}

type MoqErrorCancelled struct {
	message string
}

func NewMoqErrorCancelled() *MoqError {
	return &MoqError{err: &MoqErrorCancelled{}}
}

func (e MoqErrorCancelled) destroy() {
}

func (err MoqErrorCancelled) Error() string {
	return fmt.Sprintf("Cancelled: %s", err.message)
}

func (self MoqErrorCancelled) Is(target error) bool {
	return target == ErrMoqErrorCancelled
}

type MoqErrorClosed struct {
	message string
}

func NewMoqErrorClosed() *MoqError {
	return &MoqError{err: &MoqErrorClosed{}}
}

func (e MoqErrorClosed) destroy() {
}

func (err MoqErrorClosed) Error() string {
	return fmt.Sprintf("Closed: %s", err.message)
}

func (self MoqErrorClosed) Is(target error) bool {
	return target == ErrMoqErrorClosed
}

type MoqErrorConnect struct {
	message string
}

func NewMoqErrorConnect() *MoqError {
	return &MoqError{err: &MoqErrorConnect{}}
}

func (e MoqErrorConnect) destroy() {
}

func (err MoqErrorConnect) Error() string {
	return fmt.Sprintf("Connect: %s", err.message)
}

func (self MoqErrorConnect) Is(target error) bool {
	return target == ErrMoqErrorConnect
}

type MoqErrorCodec struct {
	message string
}

func NewMoqErrorCodec() *MoqError {
	return &MoqError{err: &MoqErrorCodec{}}
}

func (e MoqErrorCodec) destroy() {
}

func (err MoqErrorCodec) Error() string {
	return fmt.Sprintf("Codec: %s", err.message)
}

func (self MoqErrorCodec) Is(target error) bool {
	return target == ErrMoqErrorCodec
}

type MoqErrorUnauthorized struct {
	message string
}

func NewMoqErrorUnauthorized() *MoqError {
	return &MoqError{err: &MoqErrorUnauthorized{}}
}

func (e MoqErrorUnauthorized) destroy() {
}

func (err MoqErrorUnauthorized) Error() string {
	return fmt.Sprintf("Unauthorized: %s", err.message)
}

func (self MoqErrorUnauthorized) Is(target error) bool {
	return target == ErrMoqErrorUnauthorized
}

type MoqErrorLog struct {
	message string
}

func NewMoqErrorLog() *MoqError {
	return &MoqError{err: &MoqErrorLog{}}
}

func (e MoqErrorLog) destroy() {
}

func (err MoqErrorLog) Error() string {
	return fmt.Sprintf("Log: %s", err.message)
}

func (self MoqErrorLog) Is(target error) bool {
	return target == ErrMoqErrorLog
}

type FfiConverterMoqError struct{}

var FfiConverterMoqErrorINSTANCE = FfiConverterMoqError{}

func (c FfiConverterMoqError) Lift(eb RustBufferI) *MoqError {
	return LiftFromRustBuffer[*MoqError](c, eb)
}

func (c FfiConverterMoqError) Lower(value *MoqError) C.RustBuffer {
	return LowerIntoRustBuffer[*MoqError](c, value)
}

func (c FfiConverterMoqError) LowerExternal(value *MoqError) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*MoqError](c, value))
}

func (c FfiConverterMoqError) Read(reader io.Reader) *MoqError {
	errorID := readUint32(reader)

	message := FfiConverterStringINSTANCE.Read(reader)
	switch errorID {
	case 1:
		return &MoqError{&MoqErrorProtocol{message}}
	case 2:
		return &MoqError{&MoqErrorMedia{message}}
	case 3:
		return &MoqError{&MoqErrorMux{message}}
	case 4:
		return &MoqError{&MoqErrorUrl{message}}
	case 5:
		return &MoqError{&MoqErrorTimeOverflow{message}}
	case 6:
		return &MoqError{&MoqErrorLogLevel{message}}
	case 7:
		return &MoqError{&MoqErrorTask{message}}
	case 8:
		return &MoqError{&MoqErrorCancelled{message}}
	case 9:
		return &MoqError{&MoqErrorClosed{message}}
	case 10:
		return &MoqError{&MoqErrorConnect{message}}
	case 11:
		return &MoqError{&MoqErrorCodec{message}}
	case 12:
		return &MoqError{&MoqErrorUnauthorized{message}}
	case 13:
		return &MoqError{&MoqErrorLog{message}}
	default:
		panic(fmt.Sprintf("Unknown error code %d in FfiConverterMoqError.Read()", errorID))
	}

}

func (c FfiConverterMoqError) Write(writer io.Writer, value *MoqError) {
	switch variantValue := value.err.(type) {
	case *MoqErrorProtocol:
		writeInt32(writer, 1)
	case *MoqErrorMedia:
		writeInt32(writer, 2)
	case *MoqErrorMux:
		writeInt32(writer, 3)
	case *MoqErrorUrl:
		writeInt32(writer, 4)
	case *MoqErrorTimeOverflow:
		writeInt32(writer, 5)
	case *MoqErrorLogLevel:
		writeInt32(writer, 6)
	case *MoqErrorTask:
		writeInt32(writer, 7)
	case *MoqErrorCancelled:
		writeInt32(writer, 8)
	case *MoqErrorClosed:
		writeInt32(writer, 9)
	case *MoqErrorConnect:
		writeInt32(writer, 10)
	case *MoqErrorCodec:
		writeInt32(writer, 11)
	case *MoqErrorUnauthorized:
		writeInt32(writer, 12)
	case *MoqErrorLog:
		writeInt32(writer, 13)
	default:
		_ = variantValue
		panic(fmt.Sprintf("invalid error value `%v` in FfiConverterMoqError.Write", value))
	}
}

type FfiDestroyerMoqError struct{}

func (_ FfiDestroyerMoqError) Destroy(value *MoqError) {
	switch variantValue := value.err.(type) {
	case MoqErrorProtocol:
		variantValue.destroy()
	case MoqErrorMedia:
		variantValue.destroy()
	case MoqErrorMux:
		variantValue.destroy()
	case MoqErrorUrl:
		variantValue.destroy()
	case MoqErrorTimeOverflow:
		variantValue.destroy()
	case MoqErrorLogLevel:
		variantValue.destroy()
	case MoqErrorTask:
		variantValue.destroy()
	case MoqErrorCancelled:
		variantValue.destroy()
	case MoqErrorClosed:
		variantValue.destroy()
	case MoqErrorConnect:
		variantValue.destroy()
	case MoqErrorCodec:
		variantValue.destroy()
	case MoqErrorUnauthorized:
		variantValue.destroy()
	case MoqErrorLog:
		variantValue.destroy()
	default:
		_ = variantValue
		panic(fmt.Sprintf("invalid error value `%v` in FfiDestroyerMoqError.Destroy", value))
	}
}

type FfiConverterOptionalUint64 struct{}

var FfiConverterOptionalUint64INSTANCE = FfiConverterOptionalUint64{}

func (c FfiConverterOptionalUint64) Lift(rb RustBufferI) *uint64 {
	return LiftFromRustBuffer[*uint64](c, rb)
}

func (_ FfiConverterOptionalUint64) Read(reader io.Reader) *uint64 {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterUint64INSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalUint64) Lower(value *uint64) C.RustBuffer {
	return LowerIntoRustBuffer[*uint64](c, value)
}

func (c FfiConverterOptionalUint64) LowerExternal(value *uint64) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*uint64](c, value))
}

func (_ FfiConverterOptionalUint64) Write(writer io.Writer, value *uint64) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterUint64INSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalUint64 struct{}

func (_ FfiDestroyerOptionalUint64) Destroy(value *uint64) {
	if value != nil {
		FfiDestroyerUint64{}.Destroy(*value)
	}
}

type FfiConverterOptionalFloat64 struct{}

var FfiConverterOptionalFloat64INSTANCE = FfiConverterOptionalFloat64{}

func (c FfiConverterOptionalFloat64) Lift(rb RustBufferI) *float64 {
	return LiftFromRustBuffer[*float64](c, rb)
}

func (_ FfiConverterOptionalFloat64) Read(reader io.Reader) *float64 {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterFloat64INSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalFloat64) Lower(value *float64) C.RustBuffer {
	return LowerIntoRustBuffer[*float64](c, value)
}

func (c FfiConverterOptionalFloat64) LowerExternal(value *float64) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*float64](c, value))
}

func (_ FfiConverterOptionalFloat64) Write(writer io.Writer, value *float64) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterFloat64INSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalFloat64 struct{}

func (_ FfiDestroyerOptionalFloat64) Destroy(value *float64) {
	if value != nil {
		FfiDestroyerFloat64{}.Destroy(*value)
	}
}

type FfiConverterOptionalBool struct{}

var FfiConverterOptionalBoolINSTANCE = FfiConverterOptionalBool{}

func (c FfiConverterOptionalBool) Lift(rb RustBufferI) *bool {
	return LiftFromRustBuffer[*bool](c, rb)
}

func (_ FfiConverterOptionalBool) Read(reader io.Reader) *bool {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterBoolINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalBool) Lower(value *bool) C.RustBuffer {
	return LowerIntoRustBuffer[*bool](c, value)
}

func (c FfiConverterOptionalBool) LowerExternal(value *bool) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*bool](c, value))
}

func (_ FfiConverterOptionalBool) Write(writer io.Writer, value *bool) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterBoolINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalBool struct{}

func (_ FfiDestroyerOptionalBool) Destroy(value *bool) {
	if value != nil {
		FfiDestroyerBool{}.Destroy(*value)
	}
}

type FfiConverterOptionalBytes struct{}

var FfiConverterOptionalBytesINSTANCE = FfiConverterOptionalBytes{}

func (c FfiConverterOptionalBytes) Lift(rb RustBufferI) *[]byte {
	return LiftFromRustBuffer[*[]byte](c, rb)
}

func (_ FfiConverterOptionalBytes) Read(reader io.Reader) *[]byte {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterBytesINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalBytes) Lower(value *[]byte) C.RustBuffer {
	return LowerIntoRustBuffer[*[]byte](c, value)
}

func (c FfiConverterOptionalBytes) LowerExternal(value *[]byte) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*[]byte](c, value))
}

func (_ FfiConverterOptionalBytes) Write(writer io.Writer, value *[]byte) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterBytesINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalBytes struct{}

func (_ FfiDestroyerOptionalBytes) Destroy(value *[]byte) {
	if value != nil {
		FfiDestroyerBytes{}.Destroy(*value)
	}
}

type FfiConverterOptionalMoqAnnouncement struct{}

var FfiConverterOptionalMoqAnnouncementINSTANCE = FfiConverterOptionalMoqAnnouncement{}

func (c FfiConverterOptionalMoqAnnouncement) Lift(rb RustBufferI) **MoqAnnouncement {
	return LiftFromRustBuffer[**MoqAnnouncement](c, rb)
}

func (_ FfiConverterOptionalMoqAnnouncement) Read(reader io.Reader) **MoqAnnouncement {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterMoqAnnouncementINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalMoqAnnouncement) Lower(value **MoqAnnouncement) C.RustBuffer {
	return LowerIntoRustBuffer[**MoqAnnouncement](c, value)
}

func (c FfiConverterOptionalMoqAnnouncement) LowerExternal(value **MoqAnnouncement) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[**MoqAnnouncement](c, value))
}

func (_ FfiConverterOptionalMoqAnnouncement) Write(writer io.Writer, value **MoqAnnouncement) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterMoqAnnouncementINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalMoqAnnouncement struct{}

func (_ FfiDestroyerOptionalMoqAnnouncement) Destroy(value **MoqAnnouncement) {
	if value != nil {
		FfiDestroyerMoqAnnouncement{}.Destroy(*value)
	}
}

type FfiConverterOptionalMoqGroupConsumer struct{}

var FfiConverterOptionalMoqGroupConsumerINSTANCE = FfiConverterOptionalMoqGroupConsumer{}

func (c FfiConverterOptionalMoqGroupConsumer) Lift(rb RustBufferI) **MoqGroupConsumer {
	return LiftFromRustBuffer[**MoqGroupConsumer](c, rb)
}

func (_ FfiConverterOptionalMoqGroupConsumer) Read(reader io.Reader) **MoqGroupConsumer {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterMoqGroupConsumerINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalMoqGroupConsumer) Lower(value **MoqGroupConsumer) C.RustBuffer {
	return LowerIntoRustBuffer[**MoqGroupConsumer](c, value)
}

func (c FfiConverterOptionalMoqGroupConsumer) LowerExternal(value **MoqGroupConsumer) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[**MoqGroupConsumer](c, value))
}

func (_ FfiConverterOptionalMoqGroupConsumer) Write(writer io.Writer, value **MoqGroupConsumer) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterMoqGroupConsumerINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalMoqGroupConsumer struct{}

func (_ FfiDestroyerOptionalMoqGroupConsumer) Destroy(value **MoqGroupConsumer) {
	if value != nil {
		FfiDestroyerMoqGroupConsumer{}.Destroy(*value)
	}
}

type FfiConverterOptionalMoqOriginProducer struct{}

var FfiConverterOptionalMoqOriginProducerINSTANCE = FfiConverterOptionalMoqOriginProducer{}

func (c FfiConverterOptionalMoqOriginProducer) Lift(rb RustBufferI) **MoqOriginProducer {
	return LiftFromRustBuffer[**MoqOriginProducer](c, rb)
}

func (_ FfiConverterOptionalMoqOriginProducer) Read(reader io.Reader) **MoqOriginProducer {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterMoqOriginProducerINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalMoqOriginProducer) Lower(value **MoqOriginProducer) C.RustBuffer {
	return LowerIntoRustBuffer[**MoqOriginProducer](c, value)
}

func (c FfiConverterOptionalMoqOriginProducer) LowerExternal(value **MoqOriginProducer) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[**MoqOriginProducer](c, value))
}

func (_ FfiConverterOptionalMoqOriginProducer) Write(writer io.Writer, value **MoqOriginProducer) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterMoqOriginProducerINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalMoqOriginProducer struct{}

func (_ FfiDestroyerOptionalMoqOriginProducer) Destroy(value **MoqOriginProducer) {
	if value != nil {
		FfiDestroyerMoqOriginProducer{}.Destroy(*value)
	}
}

type FfiConverterOptionalMoqCatalog struct{}

var FfiConverterOptionalMoqCatalogINSTANCE = FfiConverterOptionalMoqCatalog{}

func (c FfiConverterOptionalMoqCatalog) Lift(rb RustBufferI) *MoqCatalog {
	return LiftFromRustBuffer[*MoqCatalog](c, rb)
}

func (_ FfiConverterOptionalMoqCatalog) Read(reader io.Reader) *MoqCatalog {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterMoqCatalogINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalMoqCatalog) Lower(value *MoqCatalog) C.RustBuffer {
	return LowerIntoRustBuffer[*MoqCatalog](c, value)
}

func (c FfiConverterOptionalMoqCatalog) LowerExternal(value *MoqCatalog) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*MoqCatalog](c, value))
}

func (_ FfiConverterOptionalMoqCatalog) Write(writer io.Writer, value *MoqCatalog) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterMoqCatalogINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalMoqCatalog struct{}

func (_ FfiDestroyerOptionalMoqCatalog) Destroy(value *MoqCatalog) {
	if value != nil {
		FfiDestroyerMoqCatalog{}.Destroy(*value)
	}
}

type FfiConverterOptionalMoqDimensions struct{}

var FfiConverterOptionalMoqDimensionsINSTANCE = FfiConverterOptionalMoqDimensions{}

func (c FfiConverterOptionalMoqDimensions) Lift(rb RustBufferI) *MoqDimensions {
	return LiftFromRustBuffer[*MoqDimensions](c, rb)
}

func (_ FfiConverterOptionalMoqDimensions) Read(reader io.Reader) *MoqDimensions {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterMoqDimensionsINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalMoqDimensions) Lower(value *MoqDimensions) C.RustBuffer {
	return LowerIntoRustBuffer[*MoqDimensions](c, value)
}

func (c FfiConverterOptionalMoqDimensions) LowerExternal(value *MoqDimensions) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*MoqDimensions](c, value))
}

func (_ FfiConverterOptionalMoqDimensions) Write(writer io.Writer, value *MoqDimensions) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterMoqDimensionsINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalMoqDimensions struct{}

func (_ FfiDestroyerOptionalMoqDimensions) Destroy(value *MoqDimensions) {
	if value != nil {
		FfiDestroyerMoqDimensions{}.Destroy(*value)
	}
}

type FfiConverterOptionalMoqFrame struct{}

var FfiConverterOptionalMoqFrameINSTANCE = FfiConverterOptionalMoqFrame{}

func (c FfiConverterOptionalMoqFrame) Lift(rb RustBufferI) *MoqFrame {
	return LiftFromRustBuffer[*MoqFrame](c, rb)
}

func (_ FfiConverterOptionalMoqFrame) Read(reader io.Reader) *MoqFrame {
	if readInt8(reader) == 0 {
		return nil
	}
	temp := FfiConverterMoqFrameINSTANCE.Read(reader)
	return &temp
}

func (c FfiConverterOptionalMoqFrame) Lower(value *MoqFrame) C.RustBuffer {
	return LowerIntoRustBuffer[*MoqFrame](c, value)
}

func (c FfiConverterOptionalMoqFrame) LowerExternal(value *MoqFrame) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[*MoqFrame](c, value))
}

func (_ FfiConverterOptionalMoqFrame) Write(writer io.Writer, value *MoqFrame) {
	if value == nil {
		writeInt8(writer, 0)
	} else {
		writeInt8(writer, 1)
		FfiConverterMoqFrameINSTANCE.Write(writer, *value)
	}
}

type FfiDestroyerOptionalMoqFrame struct{}

func (_ FfiDestroyerOptionalMoqFrame) Destroy(value *MoqFrame) {
	if value != nil {
		FfiDestroyerMoqFrame{}.Destroy(*value)
	}
}

type FfiConverterMapStringMoqAudio struct{}

var FfiConverterMapStringMoqAudioINSTANCE = FfiConverterMapStringMoqAudio{}

func (c FfiConverterMapStringMoqAudio) Lift(rb RustBufferI) map[string]MoqAudio {
	return LiftFromRustBuffer[map[string]MoqAudio](c, rb)
}

func (_ FfiConverterMapStringMoqAudio) Read(reader io.Reader) map[string]MoqAudio {
	result := make(map[string]MoqAudio)
	length := readInt32(reader)
	for i := int32(0); i < length; i++ {
		key := FfiConverterStringINSTANCE.Read(reader)
		value := FfiConverterMoqAudioINSTANCE.Read(reader)
		result[key] = value
	}
	return result
}

func (c FfiConverterMapStringMoqAudio) Lower(value map[string]MoqAudio) C.RustBuffer {
	return LowerIntoRustBuffer[map[string]MoqAudio](c, value)
}

func (c FfiConverterMapStringMoqAudio) LowerExternal(value map[string]MoqAudio) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[map[string]MoqAudio](c, value))
}

func (_ FfiConverterMapStringMoqAudio) Write(writer io.Writer, mapValue map[string]MoqAudio) {
	if len(mapValue) > math.MaxInt32 {
		panic("map[string]MoqAudio is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(mapValue)))
	for key, value := range mapValue {
		FfiConverterStringINSTANCE.Write(writer, key)
		FfiConverterMoqAudioINSTANCE.Write(writer, value)
	}
}

type FfiDestroyerMapStringMoqAudio struct{}

func (_ FfiDestroyerMapStringMoqAudio) Destroy(mapValue map[string]MoqAudio) {
	for key, value := range mapValue {
		FfiDestroyerString{}.Destroy(key)
		FfiDestroyerMoqAudio{}.Destroy(value)
	}
}

type FfiConverterMapStringMoqVideo struct{}

var FfiConverterMapStringMoqVideoINSTANCE = FfiConverterMapStringMoqVideo{}

func (c FfiConverterMapStringMoqVideo) Lift(rb RustBufferI) map[string]MoqVideo {
	return LiftFromRustBuffer[map[string]MoqVideo](c, rb)
}

func (_ FfiConverterMapStringMoqVideo) Read(reader io.Reader) map[string]MoqVideo {
	result := make(map[string]MoqVideo)
	length := readInt32(reader)
	for i := int32(0); i < length; i++ {
		key := FfiConverterStringINSTANCE.Read(reader)
		value := FfiConverterMoqVideoINSTANCE.Read(reader)
		result[key] = value
	}
	return result
}

func (c FfiConverterMapStringMoqVideo) Lower(value map[string]MoqVideo) C.RustBuffer {
	return LowerIntoRustBuffer[map[string]MoqVideo](c, value)
}

func (c FfiConverterMapStringMoqVideo) LowerExternal(value map[string]MoqVideo) ExternalCRustBuffer {
	return RustBufferFromC(LowerIntoRustBuffer[map[string]MoqVideo](c, value))
}

func (_ FfiConverterMapStringMoqVideo) Write(writer io.Writer, mapValue map[string]MoqVideo) {
	if len(mapValue) > math.MaxInt32 {
		panic("map[string]MoqVideo is too large to fit into Int32")
	}

	writeInt32(writer, int32(len(mapValue)))
	for key, value := range mapValue {
		FfiConverterStringINSTANCE.Write(writer, key)
		FfiConverterMoqVideoINSTANCE.Write(writer, value)
	}
}

type FfiDestroyerMapStringMoqVideo struct{}

func (_ FfiDestroyerMapStringMoqVideo) Destroy(mapValue map[string]MoqVideo) {
	for key, value := range mapValue {
		FfiDestroyerString{}.Destroy(key)
		FfiDestroyerMoqVideo{}.Destroy(value)
	}
}

const (
	uniffiRustFuturePollReady      int8 = 0
	uniffiRustFuturePollMaybeReady int8 = 1
)

type rustFuturePollFunc func(C.uint64_t, C.UniffiRustFutureContinuationCallback, C.uint64_t)
type rustFutureCompleteFunc[T any] func(C.uint64_t, *C.RustCallStatus) T
type rustFutureFreeFunc func(C.uint64_t)

//export moq_uniffiFutureContinuationCallback
func moq_uniffiFutureContinuationCallback(data C.uint64_t, pollResult C.int8_t) {
	h := cgo.Handle(uintptr(data))
	waiter := h.Value().(chan int8)
	waiter <- int8(pollResult)
}

func uniffiRustCallAsync[E any, T any, F any](
	errConverter BufReader[E],
	completeFunc rustFutureCompleteFunc[F],
	liftFunc func(F) T,
	rustFuture C.uint64_t,
	pollFunc rustFuturePollFunc,
	freeFunc rustFutureFreeFunc,
) (T, E) {
	defer freeFunc(rustFuture)

	pollResult := int8(-1)
	waiter := make(chan int8, 1)

	chanHandle := cgo.NewHandle(waiter)
	defer chanHandle.Delete()

	for pollResult != uniffiRustFuturePollReady {
		pollFunc(
			rustFuture,
			(C.UniffiRustFutureContinuationCallback)(C.moq_uniffiFutureContinuationCallback),
			C.uint64_t(chanHandle),
		)
		pollResult = <-waiter
	}

	var goValue T
	ffiValue, err := rustCallWithError(errConverter, func(status *C.RustCallStatus) F {
		return completeFunc(rustFuture, status)
	})
	if value := reflect.ValueOf(err); value.IsValid() && !value.IsZero() {
		return goValue, err
	}
	return liftFunc(ffiValue), err
}

//export moq_uniffiFreeGorutine
func moq_uniffiFreeGorutine(data C.uint64_t) {
	handle := cgo.Handle(uintptr(data))
	defer handle.Delete()

	guard := handle.Value().(chan struct{})
	guard <- struct{}{}
}

// Initialize logging with a level string: "error", "warn", "info", "debug", "trace", or "".
//
// Returns an error if called more than once.
func MoqLogLevel(level string) error {
	_, _uniffiErr := rustCallWithError[*MoqError](FfiConverterMoqError{}, func(_uniffiStatus *C.RustCallStatus) bool {
		C.uniffi_moq_ffi_fn_func_moq_log_level(FfiConverterStringINSTANCE.Lower(level), _uniffiStatus)
		return false
	})
	return _uniffiErr.AsError()
}

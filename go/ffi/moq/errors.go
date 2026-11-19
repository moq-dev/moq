package moq

import "errors"

// IsAuthError reports whether err refused the credentials: HTTP 401 or 403, an
// expired or malformed token, a protocol Unauthorized or Expired session close,
// or a track revocation. Unlike a transport failure, retrying without new
// credentials won't help, so callers should surface these rather than reconnect.
//
// MoqError lives in the generated bindings (moq.go), so this file, like cgo.go,
// only builds once those are staged alongside it.
func IsAuthError(err error) bool {
	var native *MoqError
	return errors.As(err, &native) && native != nil && MoqErrorIsAuth(native)
}

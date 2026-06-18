package moq

import "errors"

// IsAuthError reports whether err is a connection rejected by the server on
// authentication or authorization grounds: Unauthorized (HTTP 401) or Forbidden
// (HTTP 403). Unlike a transport failure, retrying without new credentials won't
// help, so callers should surface these rather than reconnect.
//
// The ErrMoqError* sentinels live in the generated bindings (moq.go), so this
// file, like cgo.go, only builds once those are staged alongside it.
func IsAuthError(err error) bool {
	return errors.Is(err, ErrMoqErrorUnauthorized) || errors.Is(err, ErrMoqErrorForbidden)
}

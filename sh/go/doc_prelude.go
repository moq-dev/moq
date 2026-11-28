// Inputs the documentation leaves to the reader. Compiled but never executed.
package docs

import (
	"context"
	"moq.dev/moq"
)

var (
	ctx            = context.Background()
	client         *moq.Client
	opusInit, rgba []byte
	packets        [][]byte
	pts            uint64
)

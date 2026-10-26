package moq_test

import (
	"context"
	"errors"
	"reflect"
	"strings"
	"testing"
	"time"

	"moq.dev/moq"
)

// awaitGrant follows auth's changes until it reports want.
func awaitGrant(t *testing.T, ctx context.Context, auth *moq.Auth, want moq.Grant) {
	t.Helper()

	for {
		got := auth.Grant()
		if got != nil && reflect.DeepEqual(*got, want) {
			return
		}
		if _, err := auth.GrantChanged(ctx); err != nil {
			t.Fatalf("grant = %+v, want %+v: %v", got, want, err)
		}
	}
}

// Both sides read the grant their peer gave them, and a peer that verifies no
// tokens of its own refuses another one as unsupported.
func TestSessionAuthReadsTheGrant(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()

	const version = "moq-lite-07-wip"
	server, err := moq.Listen(ctx, "127.0.0.1:0", moq.WithTLSGenerate("localhost"), moq.WithServerVersions(version))
	if err != nil {
		t.Fatal(err)
	}
	defer server.Close()

	accepted := make(chan *moq.Session, 1)
	implementation := make(chan *string, 1)
	go func() {
		req, err := server.Accept(ctx)
		if err != nil || req == nil {
			return
		}
		// The wrapper names itself ahead of moq-ffi, so a server can tell bindings apart.
		implementation <- req.PeerImplementation()
		session, err := req.Accept(ctx, nil, nil)
		if err != nil {
			return
		}
		accepted <- session
	}()

	client, err := moq.Dial(ctx, "https://"+server.LocalAddr(), moq.ClientConfig{
		TLS:      moq.ClientTLS{Insecure: true},
		Versions: []string{version},
		Once:     true,
	})
	if err != nil {
		t.Fatal(err)
	}
	defer client.Close()

	select {
	case got := <-implementation:
		if got == nil || !strings.HasPrefix(*got, "moq-go/") {
			t.Fatalf("peer implementation = %v, want a moq-go/ prefix", got)
		}
	case <-ctx.Done():
		t.Fatal("no request")
	}

	everything := moq.Grant{Publish: []string{"**"}, Subscribe: []string{"**"}}
	auth := client.Session().Auth()
	awaitGrant(t, ctx, auth, everything)

	var session *moq.Session
	select {
	case session = <-accepted:
	case <-ctx.Done():
		t.Fatal("no session accepted")
	}
	defer session.Cancel(0)
	awaitGrant(t, ctx, session.Auth(), everything)

	if _, err := auth.Add(ctx, "extra"); !errors.Is(err, moq.ErrUnsupported) {
		t.Fatalf("add = %v, want ErrUnsupported", err)
	}
	if got := auth.Grant(); got == nil || !reflect.DeepEqual(*got, everything) {
		t.Fatalf("grant after a refusal = %+v", got)
	}
}

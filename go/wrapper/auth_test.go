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

// awaitGrant follows session's grant until it reports want.
func awaitGrant(t *testing.T, ctx context.Context, session *moq.Session, want moq.Grant) {
	t.Helper()

	grants := session.Grants()
	defer grants.Close()
	got := grants.Grant()
	for got == nil || !reflect.DeepEqual(*got, want) {
		var err error
		if got, err = grants.Changed(ctx); err != nil {
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
	awaitGrant(t, ctx, client.Session(), everything)

	var session *moq.Session
	select {
	case session = <-accepted:
	case <-ctx.Done():
		t.Fatal("no session accepted")
	}
	defer session.Cancel(0)
	awaitGrant(t, ctx, session, everything)
	if session.Tokens() != nil {
		t.Fatal("an accepted session presents no tokens")
	}

	tokens := client.Session().Tokens()
	if err := tokens.Set("extra"); err != nil {
		t.Fatal(err)
	}
	if err := tokens.Answered(ctx); !errors.Is(err, moq.ErrUnsupported) {
		t.Fatalf("answered = %v, want ErrUnsupported", err)
	}
	grants := client.Session().Grants()
	defer grants.Close()
	if got := grants.Grant(); got == nil || !reflect.DeepEqual(*got, everything) {
		t.Fatalf("grant after a refusal = %+v", got)
	}
}

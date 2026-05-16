package moq

import "testing"

func TestNewClient(t *testing.T) {
	c := NewMoqClient()
	if c == nil {
		t.Fatal("NewMoqClient returned nil")
	}
	c.Destroy()
}

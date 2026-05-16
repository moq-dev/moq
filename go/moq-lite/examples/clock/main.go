// Publish or subscribe to a clock track. Go twin of /py/moq-lite/examples/clock.py.
//
// Each minute is a new group; each second is a frame inside that group. The
// first frame of every group is the "YYYY-MM-DD HH:MM:" prefix, followed by
// one "SS" frame per second.
//
//	go run . publish   --url https://relay.example.com --broadcast clock
//	go run . subscribe --url https://relay.example.com --broadcast clock
package main

import (
	"context"
	"flag"
	"fmt"
	"os"
	"os/signal"
	"syscall"
	"time"

	moq "github.com/moq-dev/moq/go/moq-lite"
)

func publish(ctx context.Context, url, broadcastName, trackName string, tlsVerify bool) error {
	broadcast, err := moq.NewBroadcastProducer()
	if err != nil {
		return err
	}
	defer broadcast.Close()

	track, err := broadcast.PublishTrack(trackName)
	if err != nil {
		return err
	}
	defer track.Close()

	client, err := moq.Connect(ctx, url, &moq.ClientOptions{TLSVerify: &tlsVerify})
	if err != nil {
		return err
	}
	defer client.Close()

	if err := client.Publish(broadcastName, broadcast); err != nil {
		return err
	}
	fmt.Printf("publishing %q track=%q at %s\n", broadcastName, trackName, url)

	for ctx.Err() == nil {
		now := time.Now().UTC().Truncate(time.Second)
		group, err := track.AppendGroup()
		if err != nil {
			return err
		}
		if err := group.WriteFrame([]byte(now.Format("2006-01-02 15:04:") + "")); err != nil {
			group.Close()
			return err
		}

		minute := now.Minute()
		for now.Minute() == minute && ctx.Err() == nil {
			if err := group.WriteFrame([]byte(now.Format("05"))); err != nil {
				group.Close()
				return err
			}
			select {
			case <-time.After(time.Until(now.Add(time.Second))):
			case <-ctx.Done():
			}
			now = time.Now().UTC().Truncate(time.Second)
		}
		if err := group.Finish(); err != nil {
			group.Close()
			return err
		}
		group.Close()
	}
	return ctx.Err()
}

func subscribe(ctx context.Context, url, broadcastName, trackName string, tlsVerify bool) error {
	client, err := moq.Connect(ctx, url, &moq.ClientOptions{TLSVerify: &tlsVerify})
	if err != nil {
		return err
	}
	defer client.Close()

	fmt.Printf("waiting for %q at %s\n", broadcastName, url)
	announced, err := client.AnnouncedBroadcast(broadcastName)
	if err != nil {
		return err
	}
	defer announced.Close()

	bc, err := announced.Available(ctx)
	if err != nil {
		return err
	}
	defer bc.Close()

	fmt.Printf("subscribed to %q track=%q\n", broadcastName, trackName)
	track, err := bc.SubscribeTrack(trackName)
	if err != nil {
		return err
	}
	defer track.Close()

	for group, err := range track.Groups(ctx) {
		if err != nil {
			return err
		}
		var prefix []byte
		for frame, err := range group.Frames(ctx) {
			if err != nil {
				group.Close()
				return err
			}
			if prefix == nil {
				prefix = frame
				continue
			}
			fmt.Printf("%s%s\n", prefix, frame)
		}
		group.Close()
	}
	return nil
}

func main() {
	url := flag.String("url", "", "relay URL (https://...)")
	broadcast := flag.String("broadcast", "clock", "broadcast path")
	track := flag.String("track", "seconds", "track name")
	noTLSVerify := flag.Bool("no-tls-verify", false, "disable TLS verification (dev only)")
	flag.Parse()

	if flag.NArg() != 1 || (*url == "") {
		fmt.Fprintln(os.Stderr, "usage: clock [flags] {publish|subscribe} --url URL")
		os.Exit(2)
	}

	ctx, cancel := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	defer cancel()

	tlsVerify := !*noTLSVerify
	var err error
	switch flag.Arg(0) {
	case "publish":
		err = publish(ctx, *url, *broadcast, *track, tlsVerify)
	case "subscribe":
		err = subscribe(ctx, *url, *broadcast, *track, tlsVerify)
	default:
		fmt.Fprintln(os.Stderr, "role must be publish or subscribe")
		os.Exit(2)
	}
	if err != nil && ctx.Err() == nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

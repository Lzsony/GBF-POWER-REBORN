package main

import (
	"context"
	"encoding/json"
	"flag"
	"fmt"
	"io"
	"net"
	"os"
	"os/signal"
	"path/filepath"
	"syscall"

	tea "charm.land/bubbletea/v2"
)

var version = "dev"

func run(args []string) error {
	if len(args) == 0 {
		return fmt.Errorf("usage: reborn control|gateway|admin|doctor|check-config|init-control|init-gateway|join|public-keys|version")
	}
	command := args[0]
	f := flag.NewFlagSet(command, flag.ContinueOnError)
	config := f.String("config", "", "configuration file")
	socket := f.String("socket", "/run/gbf-control/admin.sock", "local admin socket")
	dir := f.String("dir", "", "data directory")
	host := f.String("host", "", "TLS hostname or IP")
	role := f.String("role", "gateway", "control or gateway")
	input := f.Bool("json", false, "read admin request JSON from stdin")
	if err := f.Parse(args[1:]); err != nil {
		return err
	}
	switch command {
	case "version":
		fmt.Println(version)
		return nil
	case "init-control":
		if !filepath.IsAbs(*dir) {
			return fmt.Errorf("absolute --dir required")
		}
		return initControl(*dir, *host)
	case "admin":
		if *input {
			var q AdminRequest
			b, err := io.ReadAll(io.LimitReader(os.Stdin, 128<<10))
			if err != nil {
				return err
			}
			if err = decode(b, &q); err != nil {
				return err
			}
			var out json.RawMessage
			if err = adminCall(*socket, q, &out); err != nil {
				return err
			}
			fmt.Println(string(out))
			return nil
		}
		_, err := tea.NewProgram(tui{socket: *socket, width: 100, height: 30}).Run()
		return err
	}
	ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer cancel()
	if command == "control" || (*role == "control" && (command == "doctor" || command == "check-config")) {
		var c ControlConfig
		if err := loadConfig(*config, &c); err != nil {
			return err
		}
		if err := c.Validate(); err != nil {
			return err
		}
		if command == "control" {
			return serveControl(ctx, c)
		}
		if command == "check-config" {
			fmt.Println("Configuration syntax valid; files, listeners and network not checked.")
			return nil
		}
		var out json.RawMessage
		if err := adminCall(c.AdminSocket, AdminRequest{Action: "doctor"}, &out); err != nil {
			return err
		}
		fmt.Println(string(out))
		return nil
	}
	var c GatewayConfig
	if err := loadConfig(*config, &c); err != nil {
		return err
	}
	if err := c.Validate(); err != nil {
		return err
	}
	switch command {
	case "check-config":
		fmt.Println("Configuration syntax valid; files, listeners and network not checked.")
		return nil
	case "init-gateway":
		return initGateway(c)
	case "public-keys":
		v, err := exportPublic(c)
		if err == nil {
			fmt.Println(stringify(v))
		}
		return err
	case "join":
		b, err := io.ReadAll(io.LimitReader(os.Stdin, 4096))
		if err != nil {
			return err
		}
		return joinGateway(c, string(b))
	case "doctor":
		fmt.Println(stringify(gatewayDoctor(c)))
		return nil
	case "gateway":
		g, err := newGateway(c)
		if err != nil {
			return err
		}
		l, err := net.Listen("tcp", c.Listen)
		if err != nil {
			return err
		}
		return g.serve(ctx, l)
	default:
		return fmt.Errorf("unknown command")
	}
}
func main() { check(run(os.Args[1:])) }

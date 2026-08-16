// rlapi-bridge: stdin/stdout json-rpc bridge to rocket league's psynet api.
//
// usage: rlapi-bridge.exe --token <EOS_TOKEN> --account <ACCOUNT_ID> [--steam-id <ID>] [--platform steam|epic]
//
// protocol (one json object per line, stdin -> stdout):
//   -> {"id":"1","service":"Skills/GetPlayerSkill v1","body":{"PlayerID":"Steam|123|0"}}
//   <- {"id":"1","ok":true,"result":{...}}
//   <- {"id":"1","ok":false,"error":"..."}
//
// special commands:
//   {"id":"__ping__"}  -> pong (health check)
//   {"id":"__close__"} -> graceful shutdown
//   EOF / stdin close  -> shutdown

package main

import (
	"bufio"
	"context"
	"encoding/json"
	"flag"
	"fmt"
	"log/slog"
	"os"
	"strings"
	"time"

	rlapi "github.com/dank/rlapi"
)

type Request struct {
	ID      string          `json:"id"`
	Service string          `json:"service,omitempty"`
	Body    json.RawMessage `json:"body,omitempty"`
}

type Response struct {
	ID     string          `json:"id"`
	OK     bool            `json:"ok"`
	Result json.RawMessage `json:"result,omitempty"`
	Error  string          `json:"error,omitempty"`
}

func respond(id string, ok bool, result json.RawMessage, errStr string) {
	r := Response{ID: id, OK: ok, Result: result, Error: errStr}
	b, _ := json.Marshal(r)
	fmt.Println(string(b))
}

func main() {
	token := flag.String("token", "", "EOS access token (JWT)")
	account := flag.String("account", "", "Epic account ID")
	steamID := flag.String("steam-id", "", "Steam ID64 (steam platform)")
	platform := flag.String("platform", "steam", "steam or epic")
	flag.Parse()

	if *token == "" || *account == "" {
		fmt.Fprintln(os.Stderr, "usage: rlapi-bridge --token <TOKEN> --account <ACCOUNT_ID> [--steam-id <ID>] [--platform steam|epic]")
		os.Exit(1)
	}

	// Silent logger (only errors to stderr)
	slog.SetDefault(slog.New(slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{Level: slog.LevelError})))

	// Auth
	psyNet := rlapi.NewPsyNet()
	var rpc *rlapi.PsyNetRPC
	var err error

	if *platform == "steam" && *steamID != "" {
		rpc, err = psyNet.AuthPlayerSteam(*token, *account, *steamID, "")
	} else {
		rpc, err = psyNet.AuthPlayerEpic(*token, *account, "")
	}
	if err != nil {
		respond("__init__", false, nil, fmt.Sprintf("auth: %v", err))
		os.Exit(1)
	}
	defer rpc.Close()

	respond("__init__", true, json.RawMessage(`"connected"`), "")

	// Main loop
	scanner := bufio.NewScanner(os.Stdin)
	scanner.Buffer(make([]byte, 1024*1024), 1024*1024)
	ctx := context.Background()
	timeout := 20 * time.Second

	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())
		if line == "" {
			continue
		}

		var req Request
		if err := json.Unmarshal([]byte(line), &req); err != nil {
			respond("__parse__", false, nil, err.Error())
			continue
		}

		switch req.ID {
		case "__ping__":
			respond("__ping__", true, json.RawMessage(`"pong"`), "")
			continue
		case "__close__":
			respond("__close__", true, json.RawMessage(`"bye"`), "")
			return
		}

		reqCtx, cancel := context.WithTimeout(ctx, timeout)
		result, err := rpc.SendRequestRaw(reqCtx, req.Service, req.Body)
		cancel()

		if err != nil {
			respond(req.ID, false, nil, err.Error())
		} else {
			respond(req.ID, true, result, "")
		}
	}

	if err := scanner.Err(); err != nil {
		fmt.Fprintf(os.Stderr, "stdin read error: %v\n", err)
	}
}

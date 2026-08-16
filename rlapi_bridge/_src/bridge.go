// bridge.go: SendRequestRaw + AuthPlayerEpic for rlapi-bridge.
// copied into vendor/.../rlapi/ at build time. lives in _src/ so updates don't nuke it.

package rlapi

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"

	"github.com/gorilla/websocket"
)

// AuthPlayerEpic sends Platform:"Epic" (the server rejects PlatformEpic="EOS").
func (p *PsyNet) AuthPlayerEpic(authToken, accountID, accountName string) (*PsyNetRPC, error) {
	lid := NewPlayerID(PlatformEpic, accountID)
	req := &AuthPlayerRequest{
		Platform:            "Epic",
		PlayerName:          accountName,
		PlayerID:            accountID,
		Language:            "INT",
		AuthTicket:          authToken,
		BuildRegion:         "",
		FeatureSet:          p.featureSet,
		Device:              "PC",
		LocalFirstPlayerID:  lid.String(),
		SkipAuth:            false,
		SetAsPrimaryAccount: true,
		EpicAuthTicket:      authToken,
		EpicAccountID:       accountID,
	}
	var res AuthPlayerResponse
	if err := p.postJSON([]string{"Auth", "AuthPlayer", "v2"}, req, &res); err != nil {
		return nil, fmt.Errorf("auth player: %w", err)
	}
	rpc, err := p.establishSocket(res.PerConURLv2, lid, res.PsyToken, res.SessionID)
	if err != nil {
		return nil, fmt.Errorf("ws: %w", err)
	}
	go rpc.readMessages()
	rpc.schedulePing()
	return rpc, nil
}

func (p *PsyNetRPC) SendRequestRaw(ctx context.Context, service string, rawBody json.RawMessage) (json.RawMessage, error) {
	respCh, err := p.sendRequestRaw(ctx, service, rawBody)
	if err != nil {
		return nil, err
	}
	var result json.RawMessage
	if err := p.awaitResponse(ctx, respCh, &result); err != nil {
		return nil, err
	}
	return result, nil
}

func (p *PsyNetRPC) sendRequestRaw(ctx context.Context, service string, rawBody json.RawMessage) (<-chan *PsyResponse, error) {
	if !p.IsConnected() {
		return nil, fmt.Errorf("websocket connection not established")
	}
	requestID := p.requestID.getID()
	respCh := make(chan *PsyResponse, 1)

	var msg strings.Builder
	sig := generatePsySig(rawBody)
	msg.WriteString(fmt.Sprintf("PsyService: %s\r\n", service))
	msg.WriteString(fmt.Sprintf("PsyRequestID: %s\r\n", requestID))
	msg.WriteString(fmt.Sprintf("PsySig: %s\r\n", sig))
	msg.WriteString("\r\n")
	msg.Write(rawBody)

	p.mu.Lock()
	if !p.connected || p.wsConn == nil {
		p.mu.Unlock()
		return nil, fmt.Errorf("connection lost")
	}
	p.pendingReqs[requestID] = respCh
	err := p.wsConn.WriteMessage(websocket.TextMessage, []byte(msg.String()))
	if err != nil {
		delete(p.pendingReqs, requestID)
		p.mu.Unlock()
		return nil, fmt.Errorf("write: %w", err)
	}
	p.mu.Unlock()

	go func() {
		<-ctx.Done()
		p.mu.Lock()
		ch := p.pendingReqs[requestID]
		delete(p.pendingReqs, requestID)
		p.mu.Unlock()
		if ch != nil { close(ch) }
	}()
	return respCh, nil
}

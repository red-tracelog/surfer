// Web apps which integrate Surfer as an iframe can give commands to surfer via
// the .postMessage [1] function on the iframe.
//
//  For example, to tell Surfer to load waveforms from a URL, use
// `.postMessage({command: "LoadUrl", url: "https://app.surfer-project.org/picorv32.vcd"})`
//
//  For more complex functionality, one can also inject any `Message` defined
// in `surfer::Message` in surfer/main.rs. However, the API of these messages
// is not stable and may change at any time. If you add functionality via
// these, make sure to test the new functionality when changing Surfer version.
//
// [1] https://developer.mozilla.org/en-US/docs/Web/API/Window/postMessage

let wcp_relay_started = false;

function wcp_endpoint(base_url, direction) {
  const endpoint = new URL(base_url);
  endpoint.pathname = `${endpoint.pathname.replace(/\/$/, '')}/wcp/${direction}`;
  return endpoint.toString();
}

function retry_delay() {
  return new Promise(resolve => setTimeout(resolve, 1000));
}

async function start_wcp_relay(base_url) {
  if (wcp_relay_started) {
    return;
  }
  wcp_relay_started = true;
  while (true) {
    try {
      const response = await fetch(wcp_endpoint(base_url, 'viewer'), {
        method: 'POST',
        headers: {'Content-Type': 'text/plain'},
        body: base_url,
      });
      if (!response.ok) {
        throw new Error(`Surver returned ${response.status} while registering the viewer`);
      }
      break;
    } catch (error) {
      console.error('Failed to register the WCP viewer with Surver', error);
      await retry_delay();
    }
  }
  await window.surfer_start_wcp();

  (async () => {
    while (true) {
      try {
        const response = await fetch(wcp_endpoint(base_url, 'cs'));
        if (response.status === 204) {
          continue;
        }
        if (!response.ok) {
          throw new Error(`Surver returned ${response.status} while receiving a WCP message`);
        }
        await window.surfer_handle_wcp_cs_message(await response.text());
      } catch (error) {
        console.error('Failed to receive a WCP message from Surver', error);
        await retry_delay();
      }
    }
  })();

  (async () => {
    while (true) {
      const message = await window.surfer_next_wcp_sc_message();
      if (message === undefined) {
        return;
      }
      while (true) {
        try {
          const response = await fetch(wcp_endpoint(base_url, 'sc'), {
            method: 'POST',
            headers: {'Content-Type': 'text/plain'},
            body: message,
          });
          if (!response.ok) {
            throw new Error(`Surver returned ${response.status} while sending a WCP message`);
          }
          break;
        } catch (error) {
          console.error('Failed to send a WCP message to Surver', error);
          await retry_delay();
        }
      }
    }
  })();

  if (window.__surfer_host_api) {
    window.__surfer_host_api.postMessage({command: 'wcpReady'});
  }
}

async function start_host_wcp_relay() {
  if (wcp_relay_started) {
    return;
  }
  wcp_relay_started = true;
  await window.surfer_start_wcp();

  (async () => {
    while (true) {
      const message = await window.surfer_next_wcp_sc_message();
      if (message === undefined) {
        return;
      }
      if (window.__surfer_host_api) {
        window.__surfer_host_api.postMessage({command: 'wcpMessage', message});
      }
    }
  })();

  if (window.__surfer_host_api) {
    window.__surfer_host_api.postMessage({command: 'wcpReady'});
  }
}

function register_message_listener() {
  window.addEventListener("message", (event) => {
    // JSON decode the message
    const decoded = event.data

    switch (decoded.command) {
      // Load a waveform from a URL. The format is inferred from the data.
      // Example: `{command: "LoadUrl", url: "https://app.surfer-project.org/picorv32.vcd"}`

      case 'LoadUrl': {
        const msg = {
          LoadWaveformFileFromUrl: [
            decoded.url,
            "Clear"
          ]
        }
        inject_message(JSON.stringify(msg))
        break;
      }

      case 'ToggleMenu': {
        const msg = "ToggleMenu"
        inject_message(JSON.stringify(msg))
        break;
      }

      // Inject any other message supported by Surfer in the surfer::Message enum.
      // NOTE: The API of these is unstable.
      case 'InjectMessage': {
        inject_message(decoded.message);
        break
      }

      case 'StartWcp': {
        start_wcp_relay(decoded.url);
        break
      }

      case 'StartHostWcp': {
        start_host_wcp_relay();
        break
      }

      case 'WcpMessage': {
        window.surfer_handle_wcp_cs_message(decoded.message);
        break
      }

      default:
        console.log(`Unknown message.command ${decoded.command}`)
        break;
    }
  });
}

// Called by the Surfer WASM code to send a message to the host (e.g. VS Code extension).
// The host must have stored its postMessage handle in window.__surfer_host_api.
window.surfer_notify_host = function(message_json) {
  if (window.__surfer_host_api) {
    window.__surfer_host_api.postMessage(JSON.parse(message_json));
  }
};

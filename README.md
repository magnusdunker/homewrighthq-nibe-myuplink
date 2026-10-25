# myUplink for HomewrightHQ

An external integration for [HomewrightHQ](https://github.com/magnusdunker/homewrighthq) for NIBE heat pumps and other devices in myUplink.

## Installation

In HomewrightHQ: **Settings → Integrations → From GitHub**. Paste

```
https://github.com/magnusdunker/homewrighthq-nibe-myuplink
```

and press **Install**.

myUplink requires you to create your own application:

1. Log in to [dev.myuplink.com](https://dev.myuplink.com) with the account you use in the myUplink app.
2. Create an application under **Applications**. As callback URL you can enter your HomewrightHQ address followed by `/auth/external/callback`, e.g. `http://localhost:8123/auth/external/callback`.
3. Save the application's Client Identifier and Client Secret.

Then add **myUplink (NIBE)** under **Add integration**, and enter the Client Identifier and Client Secret. There are two ways to log in:

- **Client credentials** (default): HomewrightHQ logs in as the application and sees the devices that the account that created it has access to. No browser is involved.
- **Login i browseren** (log in in the browser): the next step shows an address for myUplink's login. After login, myUplink sends the browser to the callback address. That page may show an error or "Linket er udløbet" (the link has expired); that does not matter. Copy the whole address from the address bar (it contains `code=...`) and paste it into the form. An external integration cannot receive the response from `/auth/external/callback` itself.

## What you get

Each device becomes a device. If the system has only one device, the device gets the system's name. Each data point myUplink reports for the device becomes an entity with myUplink's name for it:

- measurements become sensors, with unit and type based on the unit (°C, kW, kWh, A, Hz, bar, hours ...)
- measurements with named values (e.g. "Priority": Off, Hot water, Heating) become sensors with the text as state
- settings with a range (e.g. degree minutes, heating curve offset or room temperature) become numbers that can be changed
- settings with named values (e.g. hot water demand Small, Normal, Large) become select lists

There is also an alarm sensor that is on when the system has an active alarm. If new data points appear, they get an entity at the next update. The names are fetched in Danish with `Accept-Language: da`, where myUplink has a translation.

The data points are fetched every five minutes, and the system list (alarms and connection) every half hour. Under **Options** on the integration, the time can be set from 1 to 60 minutes. If myUplink answers 429, the integration pauses for as long as myUplink asks, or 15 minutes.

## myUplink Premium

Changing settings from other programs requires the myUplink Premium (Manage) subscription on the device. Without it, myUplink answers 403, the command fails, and HomewrightHQ shows a repair issue that explains why. Reading works without Premium.

## No climate entity

The integration does not create a climate entity. Which data points are room temperature, setpoint and heat/cool depends on the model and number of climate systems, and myUplink's `smartHomeCategories` is not filled in on all models. Setpoints and offsets can be changed as numbers instead.

## Not tested against the real API

The integration is written from myUplink's API documentation and the way Home Assistant's `myuplink` integration uses it, and tested against a fake myUplink with responses shaped like the real ones. These things are not checked:

- That client credentials give access to the account's own devices with the scope `READSYSTEM WRITESYSTEM`.
- That `minValue`, `maxValue` and `stepValue` must be multiplied by `scaleValue`, while `value` is already scaled, and that a change is sent as the scaled value (e.g. `{"47398": 22.5}`).
- That select lists are sent as numbers (`{"47041": 2}`).
- That myUplink answers 403 to a change without Premium.
- Whether myUplink has a limit on the number of requests, and where it is. Five minutes per device is a cautious choice.
- Whether `Accept-Language: da` gives Danish names.

## Development

Built in Rust on the `thehome-plugin` SDK from [homewright-example-integration-rust](https://github.com/magnusdunker/homewrighthq-example-integration-rust).

- `src/api.rs`: OAuth 2.0 and the myUplink API
- `src/model.rs`: devices and data points, and how they become entities
- `src/lib.rs`: the setup and the connection to HomewrightHQ
- `tests/myuplink.rs`: tests against a fake myUplink with the responses in `tests/fixtures`, and a fake HomewrightHQ in `tests/host`

```
cargo test
cargo build --release
```

A new version is released with a tag, e.g. `git tag v0.1.0 && git push origin v0.1.0`. The workflow builds for Windows, Linux (x86_64 and arm64, static musl) and macOS and puts the binaries and `homewright.json` on the release.

## License

MIT

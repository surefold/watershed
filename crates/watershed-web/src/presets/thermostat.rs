// A greenhouse controller: fans, heating and an alert.
use watershed_core::prelude::*;

pub fn build(g: &mut Graph) -> Result<(), CalcError> {
    let temperature = g.input("temperature", 22.0)?;
    let target = g.input("target", 24.0)?;
    let tolerance = g.input("tolerance", 3.0)?;
    let tariff = g.input("tariff_per_kwh", 0.3)?;
    let night = g.input("night_mode", false)?;

    let too_hot = g.define("too_hot", temperature.gt(target + tolerance))?;
    let too_cold = g.define("too_cold", temperature.lt(target - tolerance))?;
    let fan_kw = g.define(
        "fan_kw",
        when(too_hot)
            .then(when(night).then(0.8).otherwise(1.5))
            .otherwise(0.0),
    )?;
    let heater_kw = g.define("heater_kw", when(too_cold).then(2.0).otherwise(0.0))?;
    let power_kw = g.define("power_kw", fan_kw + heater_kw)?;
    g.define("cost_per_hour", power_kw * tariff)?;
    g.define(
        "status",
        when(too_hot)
            .then("cooling")
            .otherwise(when(too_cold).then("heating").otherwise("idle")),
    )?;
    Ok(())
}

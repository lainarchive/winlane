use winlane_contracts::NetworkMode;

const DRIVER_PATHS: &[&str] = &[
    "viostor",
    "vioscsi",
    "NetKVM",
    "Balloon",
    "vioserial",
    "vioinput",
    "viorng",
    "qxldod",
    "viogpudo",
    "qemufwcfg",
];

pub fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

pub fn unattend_xml(
    computer_name: &str,
    username: &str,
    password: &str,
    network: &NetworkMode,
    image_name: Option<&str>,
    product_key: &str,
) -> String {
    let dynamic_update = matches!(network, NetworkMode::Nat);
    let computer_name = xml_escape(computer_name);
    let username = xml_escape(username);
    let password = xml_escape(password);
    let product_key = xml_escape(product_key);
    let install_from = image_name
        .map(|name| {
            format!(
                "          <InstallFrom>\n            <MetaData wcm:action=\"add\">\n              <Key>/IMAGE/NAME</Key>\n              <Value>{}</Value>\n            </MetaData>\n          </InstallFrom>\n",
                xml_escape(name)
            )
        })
        .unwrap_or_default();
    let driver_paths = DRIVER_PATHS
        .iter()
        .enumerate()
        .map(|(index, driver)| {
            format!(
                "        <PathAndCredentials wcm:action=\"add\" wcm:keyValue=\"{}\">\n          <Path>E:\\{}\\w11\\amd64</Path>\n        </PathAndCredentials>",
                index + 1,
                driver
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r##"<?xml version="1.0" encoding="utf-8"?>
<unattend xmlns="urn:schemas-microsoft-com:unattend"
  xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State"
  xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
  <settings pass="windowsPE">
    <component name="Microsoft-Windows-International-Core-WinPE" processorArchitecture="amd64" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
      <SetupUILanguage>
        <UILanguage>en-US</UILanguage>
      </SetupUILanguage>
      <InputLocale>en-US</InputLocale>
      <SystemLocale>en-US</SystemLocale>
      <UILanguage>en-US</UILanguage>
      <UserLocale>en-US</UserLocale>
    </component>
    <component name="Microsoft-Windows-Setup" processorArchitecture="amd64" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
      <Diagnostics>
        <OptIn>false</OptIn>
      </Diagnostics>
      <DiskConfiguration>
        <Disk wcm:action="add">
          <DiskID>0</DiskID>
          <WillWipeDisk>true</WillWipeDisk>
          <CreatePartitions>
            <CreatePartition wcm:action="add">
              <Order>1</Order>
              <Type>EFI</Type>
              <Size>260</Size>
            </CreatePartition>
            <CreatePartition wcm:action="add">
              <Order>2</Order>
              <Type>MSR</Type>
              <Size>16</Size>
            </CreatePartition>
            <CreatePartition wcm:action="add">
              <Order>3</Order>
              <Type>Primary</Type>
              <Extend>true</Extend>
            </CreatePartition>
          </CreatePartitions>
          <ModifyPartitions>
            <ModifyPartition wcm:action="add">
              <Order>1</Order>
              <PartitionID>1</PartitionID>
              <Label>System</Label>
              <Format>FAT32</Format>
            </ModifyPartition>
            <ModifyPartition wcm:action="add">
              <Order>2</Order>
              <PartitionID>2</PartitionID>
            </ModifyPartition>
            <ModifyPartition wcm:action="add">
              <Order>3</Order>
              <PartitionID>3</PartitionID>
              <Label>Windows</Label>
              <Letter>C</Letter>
              <Format>NTFS</Format>
            </ModifyPartition>
          </ModifyPartitions>
        </Disk>
      </DiskConfiguration>
      <DynamicUpdate>
        <Enable>{dynamic_update}</Enable>
        <WillShowUI>Never</WillShowUI>
      </DynamicUpdate>
      <ImageInstall>
        <OSImage>
{install_from}          <InstallTo>
            <DiskID>0</DiskID>
            <PartitionID>3</PartitionID>
          </InstallTo>
          <InstallToAvailablePartition>false</InstallToAvailablePartition>
        </OSImage>
      </ImageInstall>
      <UpgradeData>
        <Upgrade>false</Upgrade>
        <WillShowUI>Never</WillShowUI>
      </UpgradeData>
      <UserData>
        <AcceptEula>true</AcceptEula>
        <FullName>winlane</FullName>
        <Organization>winlane</Organization>
        <ProductKey>
          <Key>{product_key}</Key>
          <WillShowUI>Never</WillShowUI>
        </ProductKey>
      </UserData>
    </component>
    <component name="Microsoft-Windows-PnpCustomizationsWinPE" processorArchitecture="amd64" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
      <DriverPaths>
{driver_paths}
      </DriverPaths>
    </component>
  </settings>
  <settings pass="specialize">
    <component name="Microsoft-Windows-Security-SPP-UX" processorArchitecture="amd64" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
      <SkipAutoActivation>true</SkipAutoActivation>
    </component>
    <component name="Microsoft-Windows-Shell-Setup" processorArchitecture="amd64" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
      <ComputerName>{computer_name}</ComputerName>
      <TimeZone>UTC</TimeZone>
    </component>
    <component name="Microsoft-Windows-SQMApi" processorArchitecture="amd64" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
      <CEIPEnabled>0</CEIPEnabled>
    </component>
    <component name="Microsoft-Windows-Deployment" processorArchitecture="amd64" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
      <RunSynchronous>
        <RunSynchronousCommand wcm:action="add">
          <Order>1</Order>
          <Description>Allow OOBE without a network or Microsoft account</Description>
          <Path>reg.exe add HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\OOBE /v BypassNRO /t REG_DWORD /d 1 /f</Path>
        </RunSynchronousCommand>
      </RunSynchronous>
    </component>
  </settings>
  <settings pass="oobeSystem">
    <component name="Microsoft-Windows-International-Core" processorArchitecture="amd64" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
      <InputLocale>en-US</InputLocale>
      <SystemLocale>en-US</SystemLocale>
      <UILanguage>en-US</UILanguage>
      <UserLocale>en-US</UserLocale>
    </component>
    <component name="Microsoft-Windows-Shell-Setup" processorArchitecture="amd64" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
      <AutoLogon>
        <Password>
          <Value>{password}</Value>
          <PlainText>true</PlainText>
        </Password>
        <Enabled>true</Enabled>
        <LogonCount>9999999</LogonCount>
        <Username>{username}</Username>
      </AutoLogon>
      <OOBE>
        <HideEULAPage>true</HideEULAPage>
        <HideLocalAccountScreen>true</HideLocalAccountScreen>
        <HideOEMRegistrationScreen>true</HideOEMRegistrationScreen>
        <HideOnlineAccountScreens>true</HideOnlineAccountScreens>
        <HideWirelessSetupInOOBE>true</HideWirelessSetupInOOBE>
        <NetworkLocation>Work</NetworkLocation>
        <ProtectYourPC>3</ProtectYourPC>
        <SkipUserOOBE>true</SkipUserOOBE>
        <SkipMachineOOBE>true</SkipMachineOOBE>
        <VMModeOptimizations>
          <SkipWinREInitialization>true</SkipWinREInitialization>
        </VMModeOptimizations>
      </OOBE>
      <UserAccounts>
        <LocalAccounts>
          <LocalAccount wcm:action="add">
            <Password>
              <Value>{password}</Value>
              <PlainText>true</PlainText>
            </Password>
            <Description>Disposable local machine account</Description>
            <DisplayName>{username}</DisplayName>
            <Group>Administrators</Group>
            <Name>{username}</Name>
          </LocalAccount>
        </LocalAccounts>
      </UserAccounts>
      <RegisteredOrganization>winlane</RegisteredOrganization>
      <RegisteredOwner>winlane</RegisteredOwner>
      <FirstLogonCommands>
        <SynchronousCommand wcm:action="add">
          <CommandLine>cmd.exe /c &quot;for %d in (D E F G H I J K L M N O P Q R S T U V W X Y Z) do @if exist %d:\winlane-firstboot.cmd call %d:\winlane-firstboot.cmd&quot;</CommandLine>
          <Description>Install guest agents and complete first boot</Description>
          <Order>1</Order>
          <RequiresUserInput>false</RequiresUserInput>
        </SynchronousCommand>
      </FirstLogonCommands>
    </component>
  </settings>
</unattend>
"##
    )
}

pub fn firstboot_cmd() -> String {
    r##"@echo off
setlocal enableextensions
set LOG=C:\winlane-firstboot.log
echo %date% %time% firstboot start > %LOG%
set VIRTIO=
for %%D in (D E F G H I J K L) do (
  if exist %%D:\guest-agent\qemu-ga-x86_64.msi (
    set VIRTIO=%%D:
  )
)
if defined VIRTIO (
  echo virtio media %VIRTIO% >> %LOG%
  msiexec /i "%VIRTIO%\guest-agent\qemu-ga-x86_64.msi" /qn /norestart >> %LOG% 2>&1
  for %%P in (viostor vioscsi NetKVM Balloon vioserial vioinput viorng qxldod viogpudo) do (
    if exist "%VIRTIO%\%%P\w11\amd64\%%P.inf" pnputil /add-driver "%VIRTIO%\%%P\w11\amd64\%%P.inf" /install >> %LOG% 2>&1
  )
) else (
  echo virtio media not found >> %LOG%
)
powercfg -h off >> %LOG% 2>&1
net accounts /maxpwage:unlimited >> %LOG% 2>&1
echo %date% %time% ready > C:\winlane-ready.txt
endlocal
"##
    .replace('\n', "\r\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_escapes_credentials() {
        let xml = unattend_xml(
            "wl-test",
            "win&lt;lane",
            "p&amp;ss<word>",
            &NetworkMode::Isolated,
            None,
            "key",
        );
        assert!(xml.contains("win&amp;lt;lane"));
        assert!(xml.contains("p&amp;amp;ss&lt;word&gt;"));
    }

    #[test]
    fn isolated_network_disables_dynamic_update() {
        let xml = unattend_xml(
            "wl-test",
            "winlane",
            "password",
            &NetworkMode::Isolated,
            None,
            "key",
        );
        assert!(xml.contains("<Enable>false</Enable>"));
    }

    #[test]
    fn answer_file_has_uefi_disk_layout() {
        let xml = unattend_xml(
            "wl-test",
            "winlane",
            "password",
            &NetworkMode::Nat,
            Some("Windows 11 Enterprise Evaluation"),
            "key",
        );
        assert!(xml.contains("<Type>EFI</Type>"));
        assert!(xml.contains("<Type>MSR</Type>"));
        assert!(xml.contains("<PartitionID>3</PartitionID>"));
        assert!(xml.contains("<Value>Windows 11 Enterprise Evaluation</Value>"));
    }

    #[test]
    fn firstboot_installs_qemu_guest_agent() {
        assert!(firstboot_cmd().contains("qemu-ga-x86_64.msi"));
        assert!(firstboot_cmd().contains("winlane-ready.txt"));
    }
}

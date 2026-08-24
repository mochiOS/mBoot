#!/usr/bin/env perl

use strict;
use warnings;
use Cwd qw(abs_path);
use File::Basename qw(dirname);
use File::Copy qw(copy move);
use File::Path qw(make_path);
use FindBin;
use lib $FindBin::Bin;
use MbootConfig qw(read_mboot_config);

my ($config_file, $mnu_dir, $output_file);
while (@ARGV) {
    my $argument = shift @ARGV;
    if ($argument eq '--config') {
        $config_file = shift @ARGV;
    }
    elsif ($argument eq '--mnu-dir') {
        $mnu_dir = shift @ARGV;
    }
    elsif ($argument eq '--output') {
        $output_file = shift @ARGV;
    }
    else {
        die "unknown argument: $argument\n";
    }
}
defined $config_file && defined $mnu_dir && defined $output_file
    or die "usage: $0 --config FILE --mnu-dir DIR --output FILE\n";

$config_file = absolute_existing($config_file);
$mnu_dir = absolute_existing($mnu_dir);
my $mboot_dir = abs_path("$FindBin::Bin/..");
$output_file = absolute_output($output_file);
my $config = read_mboot_config($config_file);
my $cargo = $ENV{MBOOT_HOST_CARGO} // 'cargo';
for my $command ($cargo, qw(truncate mkfs.vfat mmd mcopy sgdisk dd)) {
    command_path($command) or die "required command was not found: $command\n";
}

my $output_dir = $ENV{MBOOT_OUTPUT_DIR} // "$mboot_dir/output";
$output_dir = absolute_output($output_dir);
my $work = "$output_dir/image-work";
my $target = "$output_dir/target";
my $esp = "$work/esp.img";
my $manifest = "$output_dir/launch.manifest";
make_path($work, dirname($output_file));

my $toolchain = "+$config->{toolchain}";
my $mnu_manifest = "$mnu_dir/Cargo.toml";
my $mnu_abi = "$mnu_dir/crates/abi";
my %domain_images = (
    bootstrap => {
        bin => 'domain-bootstrap',
        path => "$mnu_dir/target/x86_64-unknown-none/release/domain-bootstrap",
    },
    'event-bootstrap' => {
        bin => 'event-bootstrap',
        path => "$mnu_dir/target/x86_64-unknown-none/release/event-bootstrap",
    },
    'grant-bootstrap' => {
        bin => 'grant-bootstrap',
        path => "$mnu_dir/target/x86_64-unknown-none/release/grant-bootstrap",
    },
    'ring-bootstrap' => {
        bin => 'ring-bootstrap',
        path => "$mnu_dir/target/x86_64-unknown-none/release/ring-bootstrap",
    },
    mochios => {
        bin => 'mochios-domain',
        path => "$mnu_dir/target/x86_64-unknown-none/release/mochios-domain",
    },
    'hardware-bootstrap' => {
        bin => 'hardware-bootstrap',
        path => "$mnu_dir/target/x86_64-unknown-none/release/hardware-bootstrap",
    },
    'driver-linux' => { path => $ENV{MBOOT_DRIVER_LINUX_KERNEL} },
);
my %initramfs_images = (
    'driver-linux' => $ENV{MBOOT_DRIVER_LINUX_INITRAMFS},
);
my %required_images;
my %required_initramfs;
for my $domain (@{$config->{domains}}) {
    exists $domain_images{$domain->{image}}
        or die "no builder is available for Domain image '$domain->{image}'\n";
    $required_images{$domain->{image}} = 1;
    if ($domain->{format} eq 'linux-pvh') {
        exists $initramfs_images{$domain->{initramfs}}
            or die "no builder is available for initramfs '$domain->{initramfs}'\n";
        $required_initramfs{$domain->{initramfs}} = 1;
    }
}
if ($required_images{'driver-linux'}) {
    defined $domain_images{'driver-linux'}->{path}
        && length $domain_images{'driver-linux'}->{path}
        or die "Driver Linux kernel is required; set DRIVER_LINUX_KERNEL=/path/to/vmlinux\n";
    defined $initramfs_images{'driver-linux'}
        && length $initramfs_images{'driver-linux'}
        or die "Driver Linux initramfs is required; set DRIVER_LINUX_INITRAMFS=/path/to/initramfs.cpio\n";
    $domain_images{'driver-linux'}->{path}
        = absolute_existing($domain_images{'driver-linux'}->{path});
    $initramfs_images{'driver-linux'}
        = absolute_existing($initramfs_images{'driver-linux'});
}
my @native_images = grep { defined $domain_images{$_}->{bin} } sort keys %required_images;
if (@native_images) {
    my @bins = map { ('--bin', $domain_images{$_}->{bin}) } @native_images;
    run_env(
        { RUSTFLAGS => '-C relocation-model=static -C link-arg=-no-pie --cfg curve25519_dalek_backend="serial"' },
        $cargo, $toolchain, 'build', '-Z', 'build-std=core,alloc', '--release',
        '--target', 'x86_64-unknown-none', '--manifest-path', $mnu_manifest,
        '--no-default-features', '--features', 'domain-guest', @bins,
    );
}
for my $name (keys %required_images) {
    my $path = $domain_images{$name}->{path};
    -s $path or die "Domain image was not produced: $path\n";
}

run(
    "$mboot_dir/scripts/create-launch-manifest.pl",
    '--config', $config_file,
    map({ ('--image', "$_=$domain_images{$_}->{path}") } sort keys %required_images),
    map({ ('--initramfs', "$_=$initramfs_images{$_}") } sort keys %required_initramfs),
    '--output', $manifest,
);

run_env(
    {
        MBOOT_LAUNCH_MANIFEST => $manifest,
        RUSTFLAGS => '-C panic=abort',
    },
    $cargo, $toolchain, 'build', '-Z', 'build-std=core,alloc,compiler_builtins',
    '--release', '--target', 'x86_64-unknown-uefi', '--target-dir', $target,
    '--package', 'mboot', '--features', 'uefi-app',
    '--manifest-path', "$mboot_dir/Cargo.toml",
    '--config', qq{patch."https://github.com/mochiOS/mnu".mnu-abi.path="$mnu_abi"},
);
my $efi = "$target/x86_64-unknown-uefi/release/mboot.efi";
-s $efi or die "mBoot UEFI binary was not produced: $efi\n";

unlink $esp if -e $esp;
run('truncate', '-s', "$config->{esp_size_mib}M", $esp);
run('mkfs.vfat', '-F', '32', '-h', '2048', '-n', 'MOCHIOSHV', $esp);
local $ENV{MTOOLS_SKIP_CHECK} = 1;
run('mmd', '-i', $esp, '::/EFI');
run('mmd', '-i', $esp, '::/EFI/BOOT');
run('mmd', '-i', $esp, '::/EFI/MBOOT');
run('mcopy', '-i', $esp, $efi, '::/EFI/BOOT/BOOTX64.EFI');
run('mcopy', '-i', $esp, $manifest, '::/EFI/MBOOT/LAUNCH.MF');

my %copied_paths;
for my $domain (@{$config->{domains}}) {
    my $source = $domain_images{$domain->{image}}->{path};
    next if $copied_paths{$domain->{path}}++;
    (my $destination = $domain->{path}) =~ s{\\}{/}g;
    run('mcopy', '-i', $esp, $source, "::$destination");
    if ($domain->{format} eq 'linux-pvh') {
        my $initramfs_source = $initramfs_images{$domain->{initramfs}};
        (my $initramfs_destination = $domain->{initramfs_path}) =~ s{\\}{/}g;
        run('mcopy', '-o', '-i', $esp, $initramfs_source, "::$initramfs_destination");
    }
}

my $temporary = "$output_file.new";
unlink $temporary if -e $temporary;
run('truncate', '-s', "$config->{disk_size_mib}M", $temporary);
run(
    'sgdisk', '--clear', "--disk-guid=$config->{disk_guid}",
    '--new=1:2048:+' . $config->{esp_size_mib} . 'M', '--typecode=1:ef00',
    "--partition-guid=1:$config->{esp_guid}", '--change-name=1:mochiOS EFI',
    '--attributes=1:set:2',
    $temporary,
);
run('dd', "if=$esp", "of=$temporary", 'bs=512', 'seek=2048', 'conv=notrunc', 'status=none');
run('sgdisk', '--verify', $temporary);
move($temporary, $output_file) or die "cannot publish $output_file: $!\n";
chmod 0644, $output_file or die "cannot chmod $output_file: $!\n";
print "[done] bootable image: $output_file\n";

sub absolute_existing {
    my ($path) = @_;
    my $absolute = abs_path($path);
    defined $absolute or die "path does not exist: $path\n";
    return $absolute;
}

sub absolute_output {
    my ($path) = @_;
    return $path if $path =~ m{^/};
    return abs_path('.') . "/$path";
}

sub command_path {
    my ($command) = @_;
    return $command if $command =~ m{/} && -x $command;
    for my $directory (split /:/, $ENV{PATH} // '') {
        return "$directory/$command" if -x "$directory/$command";
    }
    return;
}

sub run {
    my (@command) = @_;
    print '+ ', join(' ', map { shell_display($_) } @command), "\n";
    system @command;
    $? == 0 or die "command failed: $command[0]\n";
}

sub run_env {
    my ($environment, @command) = @_;
    local @ENV{keys %{$environment}} = values %{$environment};
    run(@command);
}

sub shell_display {
    my ($value) = @_;
    return $value if $value =~ m{^[A-Za-z0-9_./:+,=-]+$};
    $value =~ s/'/'\\''/g;
    return "'$value'";
}
